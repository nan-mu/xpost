//! 测量 DUT 上 **hash map 查表与命中计数**，以及允许路径的合法流交付比。
//!
//! ```text
//! firewall:
//!   for fw in {hal, xdp_cag}:
//!     agent flash fw
//!     for rate in {400, 600, …, 4000}:
//!       agent insert map(blacklist) 200
//!       agent clear map(firewall_result)
//!       agent clear perf
//!       agent enable xdp
//!       # 同一 30s 窗口按目标速率发一串包。每 10 个里第 1 个是合法包。
//!       #   非法：黑名单源 IP 轮换，载荷 D
//!       #   合法：名单外源 IP，唯一递增 u16
//!       agent disable xdp
//!       agent get map(blacklist)
//!       agent get map(firewall_result)
//!       agent get map(firewall_dropped)
//!       agent get perf
//!       upload to db
//!       agent clear map(blacklist)
//!
//!   agent flash xdp_t
//!   agent insert map(blacklist) 8
//!   agent clear perf
//!   sent 1 hit
//!   agent get perf
//!   upload to db
//! ```
//!
//! 非法流源 IP 在黑名单内：XDP/HAL 命中则 DROP 并给对应键加一。
//! 合法流源 IP 不在黑名单：载荷为唯一 LE u16，DUT 写入虚拟 map `firewall_result`，不回 ACK。
//! `loss = matched / legal_sent`（集合匹配交付比）。
//! `drop_sample` 是设备缓冲拒绝的合法令牌数，不计入网络丢失。
//!
//! **数据处理**
//!
//! 每格写入 `firewall_config` + `firewall_data`（DDL：`sql/firewall.sql`）。
//!
//! `firewall_config`：
//! - `exam_id` / `firmware` / `blacklist_size` / `traffic`（混发为 `mixed`）
//! - `target_pps` / `window_s` / `started_at` / `stopped_at`
//! - `sent` — 非法流实际发送数
//! - `legal_sent` / `matched` / `loss` / `drop_sample` — 合法流统计（Trace 臂为 NULL）
//!
//! `firewall_data`：blacklist 各键命中计数。

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use crate::agent::AgentClient;
use crate::db::{self, Firmware};
use crate::exam::just_drop::PpsPath;
use crate::db::PgOpts;
use crate::pipeline::{status, BudgetEta, READY_BUDGET, RoundUi};
use crate::config::FileConfig;
use crate::pipeline::{RawTx, iface_ipv4, iface_mac};

const STIM_PORT: u16 = 8080;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const WINDOW_S: Duration = Duration::from_secs(30);
const DRAIN_MS: u64 = 200;
const MAP_NAME: &str = "blacklist";
const MAP_RESULT: &str = "firewall_result";

// Dense below ~1600 (HAL knee); keep 400-step above.
const SWEEP_PPS: &[f64] = &[
    400.0, 600.0, 800.0, 1000.0, 1200.0, 1400.0, 1600.0, 2000.0, 2400.0, 2800.0,
    3200.0, 3600.0, 4000.0,
];

pub struct FirewallOpts {
    pub formal: bool,
    pub rates: Option<&'static [f64]>,
    pub lookup: bool,
    pub skip_trace: bool,
}

fn arms_from_cfg(arms: &[String]) -> Result<Vec<Firmware>> {
    let mut out = Vec::new();
    for a in arms {
        match PpsPath::parse(a)? {
            PpsPath::Xdp => out.push(Firmware::XdpCag),
            PpsPath::Hal => out.push(Firmware::Hal),
        }
    }
    if out.is_empty() {
        bail!("firewall.toml `arms` must list at least one of xdp|hal");
    }
    out.dedup();
    Ok(out)
}

fn profile_for(fw: Firmware, lookup: bool) -> &'static str {
    match (fw, lookup) {
        (Firmware::XdpCag, true) => "firewall-cag-lookup",
        (Firmware::XdpCag, false) => "firewall-cag",
        (Firmware::XdpT, _) => "firewall-t",
        (Firmware::Hal, _) => "firewall-hal",
    }
}

fn path_for(fw: Firmware) -> PpsPath {
    match fw {
        Firmware::Hal => PpsPath::Hal,
        _ => PpsPath::Xdp,
    }
}

/// Deterministic blacklist IPs in DUT /24, avoiding DUT / gateway / host.
fn blacklist_ips(esp: Ipv4Addr, n: u32, avoid: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let o = esp.octets();
    let mut out = Vec::with_capacity(n as usize);
    let mut host = 2u8;
    while out.len() < n as usize && host < 255 {
        let ip = Ipv4Addr::new(o[0], o[1], o[2], host);
        host = host.wrapping_add(1);
        if ip == esp || avoid.contains(&ip) {
            continue;
        }
        out.push(ip);
    }
    out
}

fn miss_ip(esp: Ipv4Addr, blacklist: &[Ipv4Addr], avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let o = esp.octets();
    for host in (2u8..255).rev() {
        let ip = Ipv4Addr::new(o[0], o[1], o[2], host);
        if ip != esp && !blacklist.contains(&ip) && !avoid.contains(&ip) {
            return ip;
        }
    }
    Ipv4Addr::new(o[0], o[1], o[2], 254)
}

pub async fn run_firewall(cfg: &FileConfig, opts: FirewallOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("firewall needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let formal = opts.formal || cfg.firewall.formal;
    let window = WINDOW_S;
    let grid: &[f64] = opts.rates.unwrap_or(SWEEP_PPS);
    let arms = if opts.lookup {
        vec![Firmware::XdpCag]
    } else {
        arms_from_cfg(&cfg.firewall.arms)?
    };
    let size = cfg.firewall.blacklist_size;

    RoundUi::banner(&format!(
        "firewall hash arms={:?} blacklist={size} legal=1/10 mixed +xdp_t grid={:?} window={}s formal={formal} agent={endpoint} esp={}",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        grid.iter().map(|p| *p as u32).collect::<Vec<_>>(),
        window.as_secs(),
        cfg.target.esp_ip,
    ));

    let pg_opts = PgOpts { formal };
    let mut pg = Some(db::connect_and_ensure(pg_opts).await?);

    let plans = super::repeat::plans(grid);
    let cells = arms.len() * grid.len() * plans.len();
    let mut eta = BudgetEta::new();
    eta.add_flashes(arms.len() * plans.len() + usize::from(!opts.skip_trace));
    eta.add_fixed((READY_BUDGET + window).saturating_mul(cells as u32));
    eta.add_fixed(READY_BUDGET + Duration::from_secs(5));

    let ui = RoundUi::new();
    let exam_id = if let Some(client) = pg.as_mut() {
        ui.check_start("INSERT exam");
        let note = if opts.lookup {
            "lookup-firewall"
        } else {
            "firewall"
        };
        let id = db::insert_exam(client, Some(note)).await?;
        ui.check_ok(&format!("exam={id}"));
        Some(id)
    } else {
        None
    };

    let esp = cfg.target.esp_ip;
    let host_ip = iface_ipv4(&cfg.probe.iface).unwrap_or(esp);
    for plan in &plans {
        ui.permanent(&format!(
            "round {}/{} hal_first={} seed={}",
            plan.round,
            plans.len(),
            plan.hal_first,
            plan.seed
        ));
        let ordered = plan.order_firmware(&arms);
        for fw in &ordered {
        let profile = profile_for(*fw, opts.lookup);
        let path = path_for(*fw);
        flash_ready(cfg, &agent, &ui, &mut eta, profile).await?;

        let avoid = [esp, host_ip];
        let rules = blacklist_ips(esp, size, &avoid);
        if rules.len() != size as usize {
            bail!("could only allocate {} blacklist IPs (want {size})", rules.len());
        }
        let miss = miss_ip(esp, &rules, &avoid);

        for &pps in &plan.rates {
            // Boot leases the image. Blacklist and result maps are touched first.
            agent.disable_xdp().await.context("disable before map")?;
            ui.check_start(&format!("insert blacklist n={size}"));
            seed_blacklist(&agent, &rules).await?;
            ui.check_ok(&format!("{} entries", rules.len()));

            agent.clear_map(MAP_RESULT).await.context("clear/map firewall_result")?;
            agent.clear_perf().await.context("clear/perf")?;
            if opts.lookup {
                super::set_lookup_switch(&agent, true).await?;
            }
            agent.enable_xdp().await.context("enable/xdp")?;
            let send_begin = SystemTime::now();
            let send_mono = Instant::now();
            let started_at = send_begin;
            let phase = (plan.seed as u64) % LEGAL_EVERY;
            let (illegal_sent, legal_sent, tokens) = paced_mixed(
                cfg,
                &ui,
                &eta,
                path,
                window,
                pps,
                &rules,
                miss,
                phase,
            )
            .await?;
            let send_end = SystemTime::now();
            let send_elapsed = send_mono.elapsed();
            tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
            let drain_end = SystemTime::now();
            let export_mono = Instant::now();
            let stopped_at = drain_end;
            agent.disable_xdp().await.context("disable/xdp")?;
            let map_rows = dump_blacklist(&agent).await.context("get/map blacklist")?;
            let returned = dump_legal_tokens(&agent).await.context("get/map firewall_result")?;
            let dropped = dump_dropped(&agent).await.context("get/map firewall_dropped")?;
            let matched = match_tokens(&tokens, &returned);
            let loss = if legal_sent == 0 {
                0.0
            } else {
                matched as f64 / legal_sent as f64
            };

            ui.permanent(&format!(
                "    {} mixed @{pps:.0}: illegal={illegal_sent} legal={legal_sent} matched={matched} dropped={dropped} loss={loss:.4} map={}",
                fw.as_db(),
                map_rows.len()
            ));

            if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                ui.check_start(&format!("写入 mixed @{pps:.0}"));
                let cfg_id = db::insert_firewall_config(
                    client,
                    exam_id,
                    *fw,
                    size as i32,
                    "mixed",
                    pps,
                    window.as_secs_f64(),
                    started_at,
                    stopped_at,
                    illegal_sent as i64,
                    Some(legal_sent as i64),
                    Some(matched as i64),
                    Some(loss),
                    Some(dropped as i64),
                    Some(db::CellRepeat {
                        round: plan.round,
                        plan_seed: plan.seed,
                        hal_first: plan.hal_first,
                    }),
                    Some(db::SendSpan::from_elapsed(
                        send_begin,
                        send_end,
                        drain_end,
                        send_elapsed,
                        export_mono.elapsed(),
                    )),
                )
                .await?;
                db::insert_firewall_data(client, cfg_id, &map_rows).await?;
                let sent_bytes = encode_u16s(&tokens);
                let recv_bytes = encode_u16s(&returned);
                let sent_back = decode_u16s(&sent_bytes).context("sent token bytes")?;
                let recv_back = decode_u16s(&recv_bytes).context("recv token bytes")?;
                let recomputed = match_tokens(&sent_back, &recv_back);
                if recomputed != matched {
                    bail!("token bytes recompute {recomputed}, stored matched {matched}");
                }
                db::insert_firewall_ident(client, cfg_id, &sent_bytes, &recv_bytes).await?;
                let msg = agent.get_perf().await.context("get/perf")?;
                let decoded = db::decode_report_msg(&msg)?;
                db::write_perf(
                    client,
                    exam_id,
                    cfg_id,
                    *fw,
                    db::PerfExperiment::Firewall,
                    &decoded,
                )
                .await?;
                ui.check_ok(&format!("config={cfg_id} mask={:#x}", decoded.mask));
            }
            let _ = agent.clear_map(MAP_NAME).await;
            eta.consume_fixed(window);
            ui.set_eta(&eta.fmt());
        }
        }
    }

    if !opts.skip_trace {
        let fw = Firmware::XdpT;
        let profile = profile_for(fw, false);
        flash_ready(cfg, &agent, &ui, &mut eta, profile).await?;
        let rules = blacklist_ips(esp, 8, &[esp, host_ip]);
        seed_blacklist(&agent, &rules).await?;
        agent.clear_perf().await.context("clear/perf")?;
        agent.enable_xdp().await.context("enable/xdp")?;
        let send_begin = SystemTime::now();
        let send_mono = Instant::now();
        let started_at = send_begin;
        let sent = send_one(cfg, rules[0]).await?;
        let send_end = SystemTime::now();
        let send_elapsed = send_mono.elapsed();
        tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
        let drain_end = SystemTime::now();
        let export_mono = Instant::now();
        let stopped_at = drain_end;
        let _ = agent.disable_xdp().await;
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 trace");
            let cfg_id = db::insert_firewall_config(
                client,
                exam_id,
                fw,
                rules.len() as i32,
                "hit",
                1.0,
                0.0,
                started_at,
                stopped_at,
                sent as i64,
                None,
                None,
                None,
                None,
                None,
                Some(db::SendSpan::from_elapsed(
                    send_begin,
                    send_end,
                    drain_end,
                    send_elapsed,
                    export_mono.elapsed(),
                )),
            )
            .await?;
            let msg = agent.get_perf().await.context("get/perf")?;
            let decoded = db::decode_report_msg(&msg)?;
            db::write_perf(
                client,
                exam_id,
                cfg_id,
                fw,
                db::PerfExperiment::Firewall,
                &decoded,
            )
            .await?;
            let n_ev = decoded.trace.as_ref().map(|t| t.events.len()).unwrap_or(0);
            ui.check_ok(&format!("trace events={n_ev}"));
        }
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        db::stop_exam(client, exam_id).await?;
    }
    RoundUi::banner(&format!(
        "firewall done arms={} blacklist={size} legal=1/10 grid={}",
        arms.len(),
        grid.len()
    ));
    Ok(())
}

async fn flash_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    profile: &str,
) -> Result<()> {
    ui.begin_round();
    ui.set_eta(&eta.fmt());
    ui.check_pending(&format!("烧录 `{profile}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.check_pending("DUT ready");

    if let Err(e) = status::flash_idle(ui, agent, profile, FLASH_IDLE, eta).await {
        ui.clear_temp_for_error();
        return Err(e);
    }
    eta.consume_fixed(READY_BUDGET);
    if let Err(e) = status::settle_checklist(ui, agent, READY_BUDGET).await {
        ui.clear_temp_for_error();
        return Err(e);
    }
    ui.set_eta(&eta.fmt());

    ui.check_activate("DUT ready");
    ui.check_ok("settled");
    ui.end_round();
    let _ = cfg;
    Ok(())
}

async fn seed_blacklist(agent: &AgentClient, rules: &[Ipv4Addr]) -> Result<()> {
    agent
        .update_map_ips(MAP_NAME, rules)
        .await
        .context("update/map blacklist")?;
    Ok(())
}

async fn dump_blacklist(agent: &AgentClient) -> Result<Vec<(Ipv4Addr, i64)>> {
    let map = agent.get_map(MAP_NAME).await.context("get/map blacklist")?;
    let mut out = Vec::new();
    for e in &map.entries {
        let ip: Ipv4Addr = e
            .key
            .parse()
            .with_context(|| format!("blacklist key {:?}", e.key))?;
        let value: i64 = e
            .value
            .parse()
            .with_context(|| format!("blacklist value {:?}", e.value))?;
        out.push((ip, value));
    }
    Ok(out)
}

async fn dump_legal_tokens(agent: &AgentClient) -> Result<Vec<u16>> {
    let map = agent
        .get_map(MAP_RESULT)
        .await
        .context("get/map firewall_result")?;
    let mut out = Vec::with_capacity(map.entries.len());
    for e in &map.entries {
        let v: u64 = e
            .value
            .parse()
            .with_context(|| format!("firewall_result value {:?}", e.value))?;
        if v > u64::from(u16::MAX) {
            bail!("firewall_result value {v} exceeds u16");
        }
        out.push(v as u16);
    }
    Ok(out)
}

async fn dump_dropped(agent: &AgentClient) -> Result<u64> {
    let map = agent
        .get_map("firewall_dropped")
        .await
        .context("get/map firewall_dropped")?;
    let Some(entry) = map.entries.first() else {
        return Ok(0);
    };
    entry
        .value
        .parse()
        .with_context(|| format!("firewall_dropped value {:?}", entry.value))
}

fn match_tokens(sent: &[u16], returned: &[u16]) -> usize {
    let set: HashSet<u16> = sent.iter().copied().collect();
    let mut seen = HashSet::new();
    for &v in returned {
        if set.contains(&v) {
            seen.insert(v);
        }
    }
    seen.len()
}

const LEGAL_EVERY: u64 = 10;

/// One paced stream at `target_pps`. One packet in each group of 10 is legal.
/// `legal_phase` is that packet's index inside the group.
async fn paced_mixed(
    cfg: &FileConfig,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    target_pps: f64,
    illegal_srcs: &[Ipv4Addr],
    legal_src: Ipv4Addr,
    legal_phase: u64,
) -> Result<(u64, u64, Vec<u16>)> {
    let esp = cfg.target.esp_ip;
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => iface_mac(&cfg.probe.iface)?,
    };
    let raw = RawTx::open(
        &cfg.probe.iface,
        src_mac,
        esp_mac,
        esp,
        STIM_PORT,
        cfg.target.stim_sport,
    )
    .context("RawTx::open")?;

    let pps = target_pps.max(1.0);
    let iv = Duration::from_secs_f64(1.0 / pps);
    let expect = (pps * window.as_secs_f64()).max(1.0);
    let start = Instant::now();
    let deadline = start + window;
    let mut illegal_sent = 0u64;
    let mut legal_sent = 0u64;
    let mut illegal_i = 0usize;
    let mut next_token = 1u16;
    let mut tokens = Vec::new();
    let mut i = 0u64;
    let mut next = start;
    let mut last_paint = Instant::now() - Duration::from_secs(1);

    ui.live_set(
        &format!("{} mixed @{:.0}pps", path.as_str(), pps),
        &[("tx", 0.0, "0")],
    );
    ui.set_eta(&eta.fmt());

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if now < next {
            let sleep = next.saturating_duration_since(now);
            if sleep.is_zero() {
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(sleep.min(Duration::from_millis(5))).await;
            }
            continue;
        }

        if i % LEGAL_EVERY == legal_phase {
            let token = next_token;
            next_token = next_token
                .checked_add(1)
                .context("legal u16 token space exhausted")?;
            if raw
                .send_udp(legal_src, esp, STIM_PORT, &token.to_le_bytes())
                .is_ok()
            {
                legal_sent += 1;
                tokens.push(token);
            }
        } else {
            let src = illegal_srcs[illegal_i % illegal_srcs.len()];
            illegal_i += 1;
            if raw.send_udp(src, esp, STIM_PORT, b"D").is_ok() {
                illegal_sent += 1;
            }
        }
        i += 1;
        next += iv;
        if last_paint.elapsed() >= Duration::from_millis(100) {
            let frac = (i as f64 / expect).clamp(0.0, 1.0);
            ui.live_set(
                &format!("{} mixed @{:.0}pps", path.as_str(), pps),
                &[("tx", frac, &format!("{illegal_sent}+{legal_sent}/{i}"))],
            );
            ui.set_eta(&eta.fmt());
            last_paint = Instant::now();
        }
    }

    ui.live_clear();
    Ok((illegal_sent, legal_sent, tokens))
}

/// Trace arm: one fixed-src UDP to DUT echo port (hit path).
async fn send_one(cfg: &FileConfig, src: Ipv4Addr) -> Result<u64> {
    let esp = cfg.target.esp_ip;
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => iface_mac(&cfg.probe.iface)?,
    };
    let raw = RawTx::open(
        &cfg.probe.iface,
        src_mac,
        esp_mac,
        esp,
        STIM_PORT,
        cfg.target.stim_sport,
    )
    .context("RawTx::open")?;
    raw.send_udp(src, esp, STIM_PORT, b"D")
        .context("send_one")?;
    Ok(1)
}

pub(crate) fn encode_u16s(xs: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(xs.len() * 2);
    for x in xs {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub(crate) fn decode_u16s(bytes: &[u8]) -> Option<Vec<u16>> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::{decode_u16s, encode_u16s, match_tokens};

    #[test]
    fn stored_bytes_recompute_the_intersection() {
        let sent = [1u16, 2, 3, 4];
        let recv = [3u16, 3, 9];
        let sent_b = encode_u16s(&sent);
        let recv_b = encode_u16s(&recv);
        let sent2 = decode_u16s(&sent_b).unwrap();
        let recv2 = decode_u16s(&recv_b).unwrap();
        assert_eq!(match_tokens(&sent2, &recv2), 1);
        assert_ne!(sent_b, recv_b);
    }
}
