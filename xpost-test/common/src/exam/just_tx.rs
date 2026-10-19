//! 测量 **`XDP_TX`（及对照路径）** 的耗时。
//!
//! ```text
//! just_tx:
//!   for fw in {hal, xdp_cag}:
//!     agent flash fw
//!     for rate in {400, 800, …, 4000}:
//!       agent clear perf
//!       sent rate 30s
//!       receive all sent in 1s
//!       agent get perf
//!       upload to db
//!
//!   agent flash xdp_t
//!   agent clear perf
//!   sent 1
//!   receive all sent in 1s
//!   agent get perf
//!   upload to db
//! ```
//!
//! 主机向 DUT 的 `STIM_PORT` 发 UDP，载荷为 14 字节（`FWSTIM` + `u64` 序号）。
//! XDP 交换 MAC 与 IP 后 `XDP_TX`。主机核对回程载荷中的序号后计算 RTT；发包
//! 结束后再等待 `DRAIN`，该时间内到达的回包仍计入 RTT。HAL 对照由协议栈回显，
//! 核对与计时方式相同。
//!
//! **数据处理**
//!
//! 每格写入 `just_tx_config` + `just_tx_data`（DDL：`sql/just_tx.sql`）。
//! 仅成功回包写入 `just_tx_data`；丢包率 = `sent - ok`。
//! Perf（`agent get perf`）另落共享 / HAL CAG 表，不在此列。
//! HAL 短烟测见 `exam::dev`（`run = ["dev"]`）。
//!
//! `just_tx_config`：
//! - `exam_id` — 本轮 `exam` 行
//! - `firmware` — 当前 `fw`（`hal` / `xdp_cag` / `xdp_t`）
//! - `target_pps` — 网格 `rate`（Trace 臂为占位）
//! - `window_s` — 刺激时长（网格 30；Trace 臂为 0）
//! - `started_at` / `stopped_at` — 该格刺激墙钟起止
//! - `sent` — 主机实际发出的包数
//! - `ok` — 在 `DRAIN` 等待结束前完成序号核对并算出 RTT 的包数
//!
//! `just_tx_data`（每个成功样本一行）：
//! - `config_id` — 所属 `just_tx_config`
//! - `t_tx` — 主机发送时刻（墙钟）
//! - `t_rx` — 由核对成功的回包推得的接收时刻（RTT 落点）

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;

use crate::agent::AgentClient;
use crate::db::{self, Firmware, TxSample};
use crate::exam::just_drop::PpsPath;
use crate::db::PgOpts;
use crate::pipeline::{status, BudgetEta, READY_BUDGET, RoundUi};
use crate::config::FileConfig;
use crate::pipeline::Harness;

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const WINDOW_S: Duration = Duration::from_secs(30);
const WARMUP: Duration = Duration::from_secs(1);
const COOLDOWN: Duration = Duration::from_secs(3);
const DRAIN: Duration = Duration::from_millis(1000);
const RX_READY_TRIES: usize = 40;
const RX_READY_BURST: u32 = 64;

// Dense below ~1600 (HAL knee); keep 400-step above.
const SWEEP_PPS: &[f64] = &[
    400.0, 600.0, 800.0, 1000.0, 1200.0, 1400.0, 1600.0, 2000.0, 2400.0, 2800.0,
    3200.0, 3600.0, 4000.0,

];

#[derive(Clone, Debug)]
struct ProbeSample {
    seq: i64,
    t_tx: SystemTime,
    t_rx: Option<SystemTime>,
    saw_tx: bool,
    saw_rx: bool,
}

#[derive(Clone, Debug)]
struct WindowStats {
    target_pps: f64,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: u64,
    ok: u64,
    loss: f64,
    samples: Vec<ProbeSample>,
    device_rxcnt_delta: u64,
    max_send_gap: Duration,
    host_stalled: bool,
}

impl WindowStats {
    fn max_send_gap_ms(&self) -> f64 {
        self.max_send_gap.as_secs_f64() * 1e3
    }
}

fn profile_for(fw: Firmware) -> &'static str {
    match fw {
        Firmware::XdpCag => "just-tx-cag",
        Firmware::XdpT => "just-tx-t",
        Firmware::Hal => "just-tx-hal",
    }
}

fn path_for(fw: Firmware) -> PpsPath {
    match fw {
        Firmware::Hal => PpsPath::Hal,
        _ => PpsPath::Xdp,
    }
}

pub struct JustTxOpts {
    /// Legacy flag; formal just_tx ignores this (`dev` is handled in `exam::dev`).
    pub is_dev: bool,
    pub formal: bool,
    pub rates: Option<&'static [f64]>,
    /// Two XPOST passes per round at the given rates. Pass 0 is `perf-cag`. Pass 1 adds `perf-lookup`.
    pub lookup_pair: bool,
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
        bail!("just_tx.toml `arms` must list at least one of xdp|hal");
    }
    out.dedup();
    Ok(out)
}

pub async fn run_just_tx(cfg: &FileConfig, harness: &mut Harness, opts: JustTxOpts) -> Result<()> {
    let _ = opts.is_dev;
    let name = "just_tx";
    let formal = opts.formal;
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .with_context(|| format!("{name} needs [agent].endpoint in config/default.toml"))?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();

    let window = WINDOW_S;
    let grid: &[f64] = opts.rates.unwrap_or(SWEEP_PPS);

    // Arms from just_tx.toml; xdp_t always last.
    let arms = if opts.lookup_pair {
        vec![Firmware::XdpCag]
    } else {
        arms_from_cfg(&cfg.just_tx.arms)?
    };
    let passes = if opts.lookup_pair { 2 } else { 1 };

    RoundUi::banner(&format!(
        "{name} arms={:?} +xdp_t grid={:?} window={}s formal={formal} agent={endpoint} esp={esp_s} pg=true",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        grid.iter().map(|x| *x as i64).collect::<Vec<_>>(),
        window.as_secs(),
    ));

    let pg_opts = PgOpts { formal };
    let mut pg = Some(db::connect_and_ensure(pg_opts).await?);

    let spoof = crate::pipeline::iface_ipv4(&cfg.probe.iface)
        .unwrap_or_else(|_| gen_lan_ip(cfg.target.esp_ip, &[cfg.target.esp_ip]));
    RoundUi::banner(&format!(
        "  stim_src={spoof} (iface {} IPv4 preferred for HAL ARP)",
        cfg.probe.iface
    ));

    let plans = super::repeat::plans(grid);
    let n_flashes = arms.len() * plans.len() * passes + usize::from(!opts.skip_trace);
    let mut eta = BudgetEta::new();
    eta.add_flashes(n_flashes);
    let per_arm = READY_BUDGET + (WARMUP + window + DRAIN).saturating_mul(grid.len() as u32);
    eta.add_fixed(per_arm.saturating_mul((arms.len() * plans.len() * passes) as u32));
    eta.add_fixed(READY_BUDGET + Duration::from_secs(5)); // trace one-shot

    let ui = RoundUi::new();
    let exam_id = if let Some(client) = pg.as_mut() {
        ui.check_start("INSERT exam");
        let note = if opts.lookup_pair {
            "lookup-tx-1400"
        } else {
            "just_tx"
        };
        let id = db::insert_exam(client, Some(note)).await?;
        ui.check_ok(&format!("exam={id}"));
        Some(id)
    } else {
        None
    };

    for plan in &plans {
        ui.permanent(&format!(
            "round {}/{} hal_first={} seed={}",
            plan.round,
            plans.len(),
            plan.hal_first,
            plan.seed
        ));
        let ordered = if opts.lookup_pair {
            vec![Firmware::XdpCag, Firmware::XdpCag]
        } else {
            plan.order_firmware(&arms)
        };
        for (pass, fw) in ordered.iter().enumerate() {
        ui.begin_round();
        ui.set_eta(&eta.fmt());
        let lookup = opts.lookup_pair && pass == 1;
        let profile = if lookup {
            "just-tx-cag-lookup"
        } else {
            profile_for(*fw)
        };
        let path = path_for(*fw);
        ui.check_pending(&format!("烧录 `{profile}`"));
        ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
        ui.check_pending("DUT ready");
        flash_and_ready(cfg, &agent, &ui, &mut eta, path, spoof, profile).await?;

        for &pps in &plan.rates {
            agent.clear_perf().await.context("clear/perf")?;
            let stats = probe_window(harness, &agent, &ui, &eta, path, spoof, pps, window).await?;
            log_window(&ui, path, pps as usize, &stats);
            if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                ui.check_start("写入 postgres");
                let samples: Vec<TxSample> = stats
                    .samples
                    .iter()
                    .map(|s| TxSample {
                        seq: s.seq,
                        t_tx: s.t_tx,
                        t_rx: s.t_rx,
                        saw_tx: s.saw_tx,
                        saw_rx: s.saw_rx,
                    })
                    .collect();
                let ok = stats.ok as i64;
                let matched = samples.iter().filter(|s| s.saw_rx).count() as i64;
                let cfg_id = db::insert_just_tx_config(
                    client,
                    exam_id,
                    *fw,
                    stats.target_pps,
                    window.as_secs_f64(),
                    stats.started_at,
                    stats.stopped_at,
                    stats.sent as i64,
                    ok,
                    matched,
                    lookup,
                    Some(db::CellRepeat {
                        round: plan.round,
                        plan_seed: plan.seed,
                        hal_first: plan.hal_first,
                    }),
                )
                .await?;
                db::insert_just_tx_data(client, cfg_id, &samples).await?;
                let msg = agent.get_perf().await.context("get/perf")?;
                let decoded = db::decode_report_msg(&msg)?;
                db::write_perf(
                    client,
                    exam_id,
                    cfg_id,
                    *fw,
                    db::PerfExperiment::JustTx,
                    &decoded,
                )
                .await?;
                ui.check_ok(&format!(
                    "config={cfg_id} ok={ok}/{} mask={:#x}",
                    stats.sent,
                    decoded.mask
                ));
            }
            eta.consume_fixed(WARMUP + window + DRAIN);
            ui.set_eta(&eta.fmt());
            harness.reset_state().await;
            tokio::time::sleep(COOLDOWN).await;
        }
        ui.end_round();
        }
    }

    // Trace arm (XDP): skip when `arms` is HAL-only.
    let want_xdp_t = arms.iter().any(|a| matches!(a, Firmware::XdpCag));
    if want_xdp_t && !opts.skip_trace {
        ui.begin_round();
        ui.set_eta(&eta.fmt());
        let fw = Firmware::XdpT;
        let profile = profile_for(fw);
        let path = PpsPath::Xdp;
        ui.check_pending(&format!("烧录 `{profile}`"));
        ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
        ui.check_pending("DUT ready");
        flash_and_ready(cfg, &agent, &ui, &mut eta, path, spoof, profile).await?;
        agent.clear_perf().await.context("clear/perf")?;
        harness.reset_state().await;
        let started_at = SystemTime::now();
        let seq = harness.alloc_seq();
        let t_tx = SystemTime::now();
        let _ = harness.send_stimulus(spoof, None, seq).await;
        tokio::time::sleep(DRAIN).await;
        let sight = harness.take_seq(seq).await;
        let t0 = sight.t0_ns;
        let t1 = sight.t_bounce_ns.or(sight.t_echo_ns);
        let (got_rx, t_rx) = match (t0, t1) {
            (Some(a), Some(b)) if b >= a => {
                let rtt = Duration::from_nanos(b - a);
                (true, t_tx.checked_add(rtt))
            }
            _ => (false, None),
        };
        let stopped_at = SystemTime::now();
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 trace");
            let ok = i64::from(got_rx);
            let matched = i64::from(t1.is_some());
            let cfg_id = db::insert_just_tx_config(
                client,
                exam_id,
                fw,
                1.0,
                0.0,
                started_at,
                stopped_at,
                1,
                ok,
                matched,
                false,
                None,
            )
            .await?;
            db::insert_just_tx_data(
                client,
                cfg_id,
                &[TxSample {
                    seq: seq as i64,
                    t_tx,
                    t_rx,
                    saw_tx: t0.is_some(),
                    saw_rx: t1.is_some(),
                }],
            )
            .await?;
            let msg = agent.get_perf().await.context("get/perf")?;
            let decoded = db::decode_report_msg(&msg)?;
            db::write_perf(
                client,
                exam_id,
                cfg_id,
                fw,
                db::PerfExperiment::JustTx,
                &decoded,
            )
            .await?;
            let n_ev = decoded.trace.as_ref().map(|t| t.events.len()).unwrap_or(0);
            ui.check_ok(&format!("trace events={n_ev}"));
        }
        ui.end_round();
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        db::stop_exam(client, exam_id).await?;
    }

    RoundUi::banner(&format!("{name} done arms={} grid={}", arms.len(), grid.len()));
    Ok(())
}

fn host_stall_thresh(pps: f64) -> Duration {
    let period = Duration::from_secs_f64(1.0 / pps.max(1.0));
    (period.saturating_mul(8))
        .max(Duration::from_millis(15))
        .min(Duration::from_millis(40))
}

fn log_window(ui: &RoundUi, path: PpsPath, pair_id: usize, stats: &WindowStats) {
    ui.permanent(&format!(
        "    {} pair={pair_id}: sent={} ok={} loss={:.2}% rxcntΔ={} max_gap={:.1}ms{}",
        path.as_str(),
        stats.sent,
        stats.ok,
        100.0 * stats.loss,
        stats.device_rxcnt_delta,
        stats.max_send_gap_ms(),
        if stats.host_stalled { " HOST_STALL" } else { "" },
    ));
    let slack = (stats.sent / 50).max(8);
    if stats.device_rxcnt_delta + 2 < stats.ok {
        ui.permanent(&format!(
            "    WARN: device rxcntΔ={} < host ok={}",
            stats.device_rxcnt_delta, stats.ok
        ));
    } else if stats.device_rxcnt_delta > stats.ok + slack {
        ui.permanent(&format!(
            "    WARN: device rxcntΔ={} ≫ host ok={}",
            stats.device_rxcnt_delta, stats.ok
        ));
    }
}


async fn flash_and_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    stim_src: Ipv4Addr,
    profile: &str,
) -> Result<()> {
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
    wait_rx_ready(cfg, agent, ui, path, stim_src).await?;
    ui.check_ok("rx ready");
    Ok(())
}


async fn probe_window(
    harness: &mut Harness,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    spoof: Ipv4Addr,
    target_pps: f64,
    window: Duration,
) -> Result<WindowStats> {
    let pps = target_pps.max(1.0);
    let interval = Duration::from_secs_f64(1.0 / pps);
    let stall_thresh = host_stall_thresh(pps);

    if !WARMUP.is_zero() {
        harness.reset_state().await;
        let warm_deadline = Instant::now() + WARMUP;
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;
        while Instant::now() < warm_deadline {
            ticker.tick().await;
            if Instant::now() >= warm_deadline {
                break;
            }
            let seq = harness.alloc_seq();
            let _ = harness.send_stimulus(spoof, None, seq).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        harness.reset_state().await;
    }

    let before = agent.map_slot("rxcnt", IPPROTO_UDP).await?;
    let _ = agent.clear_perf().await;
    harness.reset_state().await;
    let started_at = SystemTime::now();
    let start = Instant::now();
    let deadline = start + window;
    let mut seqs: Vec<(u64, SystemTime)> = Vec::new();
    let mut last_paint = Instant::now() - Duration::from_secs(1);
    let mut last_tx: Option<Instant> = None;
    let mut max_send_gap = Duration::ZERO;
    let mut host_stalled = false;
    let expect = (pps * window.as_secs_f64()).max(1.0);

    // Delay (not Burst): late ticks skip catch-up; avoids 80–180ms gap → burst pairs.
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;

    while Instant::now() < deadline {
        ticker.tick().await;
        if Instant::now() >= deadline {
            break;
        }
        let seq = harness.alloc_seq();
        let t_mono = Instant::now();
        let t_wall = SystemTime::now();
        if let Some(prev) = last_tx {
            let gap = t_mono.saturating_duration_since(prev);
            if gap > max_send_gap {
                max_send_gap = gap;
            }
            if gap > stall_thresh {
                host_stalled = true;
            }
        }
        if harness.send_stimulus(spoof, None, seq).await.is_ok() {
            seqs.push((seq, t_wall));
            last_tx = Some(t_mono);
        }

        if last_paint.elapsed() >= Duration::from_millis(100) {
            let sent = seqs.len() as f64;
            let tx_frac = (sent / expect).clamp(0.0, 1.0);
            let t_frac = (start.elapsed().as_secs_f64() / window.as_secs_f64()).clamp(0.0, 1.0);
            ui.live_set(
                &format!("{} @{:.0} pps", path.as_str(), pps),
                &[(
                    "tx",
                    tx_frac.max(t_frac * 0.01),
                    &format!("{}/{}", seqs.len(), expect as u64),
                )],
            );
            ui.set_eta(&eta.fmt());
            last_paint = Instant::now();
        }
    }

    ui.live_clear();
    ui.check_start("收集回包");
    tokio::time::sleep(DRAIN).await;

    let mut samples = Vec::with_capacity(seqs.len());
    let mut ok_n = 0u64;
    let mut n_t0 = 0u64;
    let mut n_t1 = 0u64;
    let sent_n = seqs.len() as u64;
    for (i, (seq, t_tx)) in seqs.iter().enumerate() {
        let sight = harness.take_seq(*seq).await;
        let t0 = sight.t0_ns;
        let t1 = sight.t_bounce_ns.or(sight.t_echo_ns);
        if t0.is_some() {
            n_t0 += 1;
        }
        if t1.is_some() {
            n_t1 += 1;
        }
        let (got_rx, t_rx) = match (t0, t1) {
            (Some(a), Some(b)) if b >= a => {
                let rtt = Duration::from_nanos(b - a);
                (true, t_tx.checked_add(rtt))
            }
            _ => (false, None),
        };
        if got_rx {
            ok_n += 1;
        }
        samples.push(ProbeSample {
            seq: i64::try_from(*seq).unwrap_or(i64::MAX),
            t_tx: *t_tx,
            t_rx,
            saw_tx: t0.is_some(),
            saw_rx: t1.is_some(),
        });
        if i % 32 == 0 || i + 1 == seqs.len() {
            ui.check_note(&format!("{ok_n}/{sent_n} ok"));
        }
    }
    ui.check_ok(&format!("{ok_n}/{sent_n} ok"));

    let after = agent.map_slot("rxcnt", IPPROTO_UDP).await?;
    let sent = seqs.len() as u64;
    let loss = if sent == 0 {
        1.0
    } else {
        1.0 - (ok_n as f64 / sent as f64)
    };
    let stopped_at = SystemTime::now();
    if ok_n == 0 && sent > 0 {
        ui.permanent(&format!(
            "    diag: sent={sent} t0={n_t0} t1={n_t1} rxcntΔ={}",
            after.saturating_sub(before)
        ));
    }
    Ok(WindowStats {
        target_pps: pps,
        started_at,
        stopped_at,
        sent,
        ok: ok_n,
        loss,
        samples,
        device_rxcnt_delta: after.saturating_sub(before),
        max_send_gap,
        host_stalled,
    })
}


async fn wait_rx_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    path: PpsPath,
    stim_src: Ipv4Addr,
) -> Result<()> {
    let esp = cfg.target.esp_ip;
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => crate::pipeline::iface_mac(&cfg.probe.iface)?,
    };
    let raw = std::sync::Arc::new(crate::pipeline::RawTx::open(
        &cfg.probe.iface,
        src_mac,
        esp_mac,
        esp,
        STIM_PORT,
        cfg.target.stim_sport,
    )?);
    for i in 0..RX_READY_TRIES {
        let before = agent.map_slot("rxcnt", IPPROTO_UDP).await?;
        let n = RX_READY_BURST;
        let raw_c = raw.clone();
        tokio::task::spawn_blocking(move || {
            for _ in 0..n {
                let _ = raw_c.send_udp(stim_src, esp, STIM_PORT, b"READY");
            }
        })
        .await
        .context("RX ready burst")?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let after = agent.map_slot("rxcnt", IPPROTO_UDP).await?;
        if after > before {
            ui.check_note(&format!("rxcnt {before}→{after}"));
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(());
        }
        if i % 5 == 4 {
            ui.check_note(&format!("try{i} quiet rxcnt={after}"));
        }
    }
    bail!("DUT never counted RX after flash ({})", path.as_str());
}


fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let o = esp.octets();
    let mut rng = rand::rng();
    for _ in 0..64 {
        let last = rng.random_range(2u8..=254);
        let ip = Ipv4Addr::new(o[0], o[1], o[2], last);
        if !avoid.contains(&ip) && ip != esp {
            return ip;
        }
    }
    Ipv4Addr::new(o[0], o[1], o[2], if o[3] == 200 { 201 } else { 200 })
}
