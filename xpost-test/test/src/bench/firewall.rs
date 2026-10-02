//! Formal `firewall` — hash-map blacklist size × hit/miss PPS grid.
//!
//! Arms: `xdp_cag` + `hal`, then `xdp_t` Trace. Rules seeded via MapCrudOp
//! (UDP `:8081`) before each size; dataplane is lookup-only.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use test_common::map_crud_op::{self, MapCrudOp};

use crate::agent::AgentClient;
use crate::bench::formal::{self, Firmware};
use crate::bench::just_drop_pps::PpsPath;
use crate::bench::pg_dsn::PgOpts;
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::config::FileConfig;
use crate::harness::Harness;
use crate::raw_tx::{RawTx, iface_ipv4, iface_mac};

const STIM_PORT: u16 = 8080;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const WINDOW_S: Duration = Duration::from_secs(30);
const DRAIN_MS: u64 = 200;
const MAP_NAME: &str = "blacklist";

const SWEEP_PPS: &[f64] = &[
    400.0, 800.0, 1200.0, 1600.0, 2000.0, 2400.0, 2800.0, 3200.0, 3600.0, 4000.0,
];

pub struct FirewallOpts {
    pub formal: bool,
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

fn profile_for(fw: Firmware) -> &'static str {
    match fw {
        Firmware::XdpCag => "firewall-cag",
        Firmware::XdpT => "firewall-t",
        Firmware::Hal => "firewall-hal-cag",
    }
}

fn path_for(fw: Firmware) -> PpsPath {
    match fw {
        Firmware::Hal => PpsPath::Hal,
        _ => PpsPath::Xdp,
    }
}

fn ip_key(ip: Ipv4Addr) -> [u8; 4] {
    ip.octets()
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
    let grid: &[f64] = SWEEP_PPS;
    let arms = arms_from_cfg(&cfg.firewall.arms)?;
    let sizes = cfg.firewall.sizes.clone();
    let traffic_modes = cfg.firewall.traffic.clone();

    RoundUi::banner(&format!(
        "firewall hash arms={:?} sizes={sizes:?} traffic={traffic_modes:?} +xdp_t grid={:?} window={}s formal={formal} agent={endpoint} esp={}",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        grid.iter().map(|p| *p as u32).collect::<Vec<_>>(),
        window.as_secs(),
        cfg.target.esp_ip,
    ));

    let pg_opts = PgOpts { formal };
    let mut pg = Some(formal::connect_and_ensure(pg_opts).await?);

    let cells = arms.len() * sizes.len() * traffic_modes.len() * grid.len();
    let mut eta = BudgetEta::new();
    eta.add_flashes(arms.len() + 1);
    eta.add_fixed((READY_BUDGET + window).saturating_mul(cells as u32));
    eta.add_fixed(READY_BUDGET + Duration::from_secs(5));

    let ui = RoundUi::new();
    let exam_id = if let Some(client) = pg.as_mut() {
        ui.check_start("INSERT exam");
        let id = formal::insert_exam(client, Some("firewall")).await?;
        ui.check_ok(&format!("exam={id}"));
        Some(id)
    } else {
        None
    };

    let esp = cfg.target.esp_ip;
    let host_ip = iface_ipv4(&cfg.probe.iface).unwrap_or(esp);
    let mut harness = open_harness(cfg).await?;

    for fw in &arms {
        let profile = profile_for(*fw);
        let path = path_for(*fw);
        flash_ready(cfg, &agent, &ui, &mut eta, profile).await?;

        for &size in &sizes {
            let avoid = [esp, host_ip];
            let rules = blacklist_ips(esp, size, &avoid);
            if rules.len() != size as usize {
                bail!("could only allocate {} blacklist IPs (want {size})", rules.len());
            }
            let miss = miss_ip(esp, &rules, &avoid);

            ui.check_start(&format!("seed blacklist n={size}"));
            seed_blacklist(&mut harness, host_ip, &rules).await?;
            ui.check_ok(&format!("{} entries", rules.len()));

            // Functional probe (one packet each).
            probe_rule(&mut harness, rules[0], true).await?;
            probe_rule(&mut harness, miss, false).await?;

            for traffic in &traffic_modes {
                let hit = traffic == "hit";
                let src = if hit { rules[0] } else { miss };
                for &pps in grid {
                    agent.perf_clear().await.context("perf_clear")?;
                    let started_at = SystemTime::now();
                    let sent =
                        paced_fixed_src(cfg, &ui, &eta, path, window, pps, src).await?;
                    tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
                    let stopped_at = SystemTime::now();

                    ui.permanent(&format!(
                        "    {} size={size} {traffic} @{pps:.0}: sent={sent} src={src}",
                        fw.as_db()
                    ));

                    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                        ui.check_start(&format!("写入 size={size} {traffic} @{pps:.0}"));
                        let cfg_id = formal::insert_firewall_config(
                            client,
                            exam_id,
                            *fw,
                            size as i32,
                            traffic,
                            pps,
                            window.as_secs_f64(),
                            started_at,
                            stopped_at,
                            sent as i64,
                        )
                        .await?;
                        let msg = agent.request("report").await.context("report")?;
                        let decoded = formal::decode_report_msg(&msg)?;
                        formal::write_perf(
                            client,
                            exam_id,
                            cfg_id,
                            *fw,
                            formal::PerfExperiment::Firewall,
                            &decoded,
                        )
                        .await?;
                        ui.check_ok(&format!("config={cfg_id} mask={:#x}", decoded.mask));
                    }
                    eta.consume_fixed(window);
                    ui.set_eta(&eta.fmt());
                }
            }

            // Clear rules before next size (serial bulk clear).
            let _ = agent.request("blacklist clear").await;
        }
    }

    // Trace one-shot (seed a tiny blacklist for a representative hit path).
    {
        let fw = Firmware::XdpT;
        let profile = profile_for(fw);
        flash_ready(cfg, &agent, &ui, &mut eta, profile).await?;
        let rules = blacklist_ips(esp, 8, &[esp, host_ip]);
        seed_blacklist(&mut harness, host_ip, &rules).await?;
        agent.perf_clear().await.context("perf_clear")?;
        let started_at = SystemTime::now();
        let sent = paced_fixed_src(
            cfg,
            &ui,
            &eta,
            PpsPath::Xdp,
            Duration::from_millis(50),
            20.0,
            rules[0],
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
        let stopped_at = SystemTime::now();
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 trace");
            let cfg_id = formal::insert_firewall_config(
                client,
                exam_id,
                fw,
                rules.len() as i32,
                "hit",
                1.0,
                0.0,
                started_at,
                stopped_at,
                sent.max(1) as i64,
            )
            .await?;
            let msg = agent.request("report").await.context("report")?;
            let decoded = formal::decode_report_msg(&msg)?;
            formal::write_perf(
                client,
                exam_id,
                cfg_id,
                fw,
                formal::PerfExperiment::Firewall,
                &decoded,
            )
            .await?;
            let n_ev = decoded.trace.as_ref().map(|t| t.events.len()).unwrap_or(0);
            ui.check_ok(&format!("trace events={n_ev}"));
        }
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        formal::stop_exam(client, exam_id).await?;
    }
    RoundUi::banner(&format!(
        "firewall done arms={} sizes={} traffic={} grid={}",
        arms.len(),
        sizes.len(),
        traffic_modes.len(),
        grid.len()
    ));
    Ok(())
}

async fn open_harness(cfg: &FileConfig) -> Result<Harness> {
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => iface_mac(&cfg.probe.iface)?,
    };
    let raw = RawTx::open(
        &cfg.probe.iface,
        src_mac,
        esp_mac,
        cfg.target.esp_ip,
        cfg.target.echo_port,
        cfg.target.stim_sport,
    )?;
    Harness::start(
        cfg.probe.iface.clone(),
        cfg.target.esp_ip,
        Some(cfg.target.vip_ip),
        cfg.target.echo_port,
        cfg.target.reply_port,
        cfg.target.ctrl_port,
        cfg.probe.pair_timeout_ms,
        raw,
    )
    .await
    .context("start harness")
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
    for i in 0..20 {
        match agent.request("ping").await {
            Ok(m) if m.contains("OK ping") => {
                ui.check_ok("ping");
                break;
            }
            Ok(m) => ui.check_note(&format!("ping try{i}: {m}")),
            Err(e) => ui.check_note(&format!("ping try{i}: {e:#}")),
        }
        if i == 19 {
            ui.check_fail("no ping");
            ui.clear_temp_for_error();
            bail!("DUT not responding to ping after flash `{profile}`");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    ui.end_round();
    let _ = cfg;
    Ok(())
}

async fn seed_blacklist(harness: &mut Harness, ctrl_src: Ipv4Addr, rules: &[Ipv4Addr]) -> Result<()> {
    // Clear first (best-effort via MapCrud DELETE of known keys is slow; serial clear is done by caller).
    for ip in rules {
        let key = ip_key(*ip);
        let ack = harness
            .map_crud_op(
                ctrl_src,
                MapCrudOp::simple(
                    map_crud_op::KIND_UPDATE,
                    MAP_NAME,
                    map_crud_op::MAP_TYPE_HASH,
                    &key,
                    &[1],
                    0,
                ),
            )
            .await
            .with_context(|| format!("MapCrud UPDATE {ip}"))?;
        if ack.status != map_crud_op::STATUS_OK {
            bail!("MapCrud UPDATE {ip} status={} {}", ack.status, ack.reason);
        }
    }
    Ok(())
}

async fn probe_rule(harness: &mut Harness, src: Ipv4Addr, expect_drop: bool) -> Result<()> {
    harness.reset_state().await;
    let seq = harness.alloc_seq();
    harness.send_stimulus(src, None, seq).await?;
    let _ = harness.wait_t0(seq, None).await?;
    let echo = harness
        .wait_echo(seq, Some(Duration::from_millis(800)))
        .await
        .unwrap_or(None);
    let got_echo = echo.is_some();
    if expect_drop && got_echo {
        bail!("probe {src}: expected drop (no ACK) but got echo");
    }
    if !expect_drop && !got_echo {
        bail!("probe {src}: expected pass (ACK) but got none");
    }
    Ok(())
}

async fn paced_fixed_src(
    cfg: &FileConfig,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    target_pps: f64,
    src: Ipv4Addr,
) -> Result<u64> {
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
    let interval = Duration::from_secs_f64(1.0 / pps);
    let expect = (pps * window.as_secs_f64()).max(1.0);
    let start = Instant::now();
    let deadline = start + window;
    let mut sent = 0u64;
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    ui.live_set(
        &format!("{} @{:.0}pps", path.as_str(), pps),
        &[("tx", 0.0, "0")],
    );
    ui.set_eta(&eta.fmt());
    let mut last_paint = Instant::now() - Duration::from_secs(1);

    while Instant::now() < deadline {
        ticker.tick().await;
        if Instant::now() >= deadline {
            break;
        }
        if raw.send_udp(src, esp, STIM_PORT, b"D").is_ok() {
            sent += 1;
        }
        if last_paint.elapsed() >= Duration::from_millis(100) {
            let frac = (sent as f64 / expect).clamp(0.0, 1.0);
            ui.live_set(
                &format!("{} @{:.0}pps", path.as_str(), pps),
                &[("tx", frac, &format!("{sent}/{expect:.0}"))],
            );
            ui.set_eta(&eta.fmt());
            last_paint = Instant::now();
        }
    }
    ui.live_clear();
    Ok(sent)
}
