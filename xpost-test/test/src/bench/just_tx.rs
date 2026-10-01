//! `just_tx` — formal exam: XDP CAG / HAL CAG full pps grid + XDP Trace one-shot.
//!
//! Formal (`window_s=30s`): flash → perf-clear → RTT window per pps → decode report → ORM.
//! No OP early-stop, no same-pps repeats, no `report_raw`, no open-run DROP/TRUNCATE.
//!
//! `dev`: flame Trace only → `/tmp/xpost-dev-flame.report` (no Postgres).

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;

use crate::agent::AgentClient;
use crate::bench::counters;
use crate::bench::formal::{self, Firmware, TxSample};
use crate::bench::just_drop_pps::PpsPath;
use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::config::FileConfig;
use crate::harness::Harness;

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const WINDOW_S: Duration = Duration::from_secs(30);
const WINDOW_SMOKE: Duration = Duration::from_secs(4);
const WARMUP: Duration = Duration::from_secs(1);
const COOLDOWN: Duration = Duration::from_secs(3);
const DRAIN: Duration = Duration::from_millis(1000);
const DRAIN_MS: u64 = 200;
const RX_READY_TRIES: usize = 40;
const RX_READY_BURST: u32 = 64;

const SWEEP_PPS: &[f64] = &[
    400.0, 800.0, 1200.0, 1600.0, 2000.0, 2400.0, 2800.0, 3200.0, 3600.0, 4000.0,
];
const SWEEP_SMOKE_PPS: &[f64] = &[400.0, 1200.0, 2400.0, 4000.0];

const DEV_FLAME_PPS: f64 = 100.0;
const DEV_FLAME_WINDOW: Duration = Duration::from_secs(2);
const DEV_FLAME_REPORT_PATH: &str = "/tmp/xpost-dev-flame.report";

#[derive(Clone, Debug)]
struct ProbeSample {
    t_tx: SystemTime,
    t_rx: Option<SystemTime>,
    got_rx: bool,
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
    n_t0: u64,
    counters: serde_json::Value,
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
        Firmware::Hal => "just-tx-hal-cag",
    }
}

fn path_for(fw: Firmware) -> PpsPath {
    match fw {
        Firmware::Hal => PpsPath::Hal,
        _ => PpsPath::Xdp,
    }
}

pub struct JustTxOpts {
    /// Legacy filter: if non-empty, maps xdp→XdpCag, hal→Hal (XdpT always runs last when formal).
    pub paths: Vec<PpsPath>,
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub rounds: Option<usize>,
    pub tables: TableSet,
    /// Explicit `TRUNCATE exam CASCADE` before this exam.
    pub formal_clear: bool,
}

fn exp_name(tables: TableSet) -> &'static str {
    match tables {
        TableSet::Dev => "dev",
        _ => "just_tx",
    }
}


pub async fn run_just_tx(cfg: &FileConfig, harness: &mut Harness, opts: JustTxOpts) -> Result<()> {
    let name = exp_name(opts.tables);
    let is_dev = matches!(opts.tables, TableSet::Dev);
    let formal = if is_dev {
        false
    } else {
        opts.formal || cfg.just_tx.formal
    };
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .with_context(|| format!("{name} needs [agent].endpoint in config/default.toml"))?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();

    let window = opts
        .window_secs
        .map(Duration::from_secs)
        .unwrap_or(if opts.smoke { WINDOW_SMOKE } else { WINDOW_S });
    let grid: &[f64] = if opts.smoke { SWEEP_SMOKE_PPS } else { SWEEP_PPS };

    if is_dev {
        return run_dev_flame(cfg, harness, &agent, &endpoint, &esp_s).await;
    }

    // Arms: xdp_cag + hal (filterable via opts.paths); xdp_t always last.
    let mut arms: Vec<Firmware> = Vec::new();
    if opts.paths.is_empty() {
        arms.extend([Firmware::XdpCag, Firmware::Hal]);
    } else {
        for p in &opts.paths {
            match p {
                PpsPath::Xdp => arms.push(Firmware::XdpCag),
                PpsPath::Hal => arms.push(Firmware::Hal),
            }
        }
    }
    arms.dedup();

    RoundUi::banner(&format!(
        "{name} arms={:?} +xdp_t grid={:?} window={}s formal={formal} clear={} agent={endpoint} esp={esp_s} pg={}",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        grid.iter().map(|x| *x as i64).collect::<Vec<_>>(),
        window.as_secs(),
        opts.formal_clear,
        !opts.no_pg
    ));

    let mut pg = if opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts { formal };
        let client = formal::connect_and_ensure(pg_opts).await?;
        if opts.formal_clear {
            formal::clear(&client, pg_opts).await?;
        }
        Some(client)
    };

    let spoof = crate::raw_tx::iface_ipv4(&cfg.probe.iface)
        .unwrap_or_else(|_| gen_lan_ip(cfg.target.esp_ip, &[cfg.target.esp_ip]));
    RoundUi::banner(&format!(
        "  stim_src={spoof} (iface {} IPv4 preferred for HAL ARP)",
        cfg.probe.iface
    ));

    let n_flashes = arms.len() + 1; // + xdp_t
    let mut eta = BudgetEta::new();
    eta.add_flashes(n_flashes);
    let per_arm = READY_BUDGET + (WARMUP + window + DRAIN).saturating_mul(grid.len() as u32);
    eta.add_fixed(per_arm.saturating_mul(arms.len() as u32));
    eta.add_fixed(READY_BUDGET + Duration::from_secs(5)); // trace one-shot

    let ui = RoundUi::new();
    let exam_id = if let Some(client) = pg.as_mut() {
        ui.check_start("INSERT exam");
        let id = formal::insert_exam(client, Some("just_tx")).await?;
        ui.check_ok(&format!("exam={id}"));
        Some(id)
    } else {
        None
    };

    for fw in &arms {
        ui.begin_round();
        ui.set_eta(&eta.fmt());
        let profile = profile_for(*fw);
        let path = path_for(*fw);
        ui.check_pending(&format!("烧录 `{profile}`"));
        ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
        ui.check_pending("DUT ready");
        flash_and_ready(cfg, &agent, &ui, &mut eta, path, spoof, profile).await?;

        for &pps in grid {
            agent.perf_clear().await.context("perf_clear")?;
            let stats = probe_window(harness, &agent, &ui, &eta, path, spoof, pps, window).await?;
            log_window(&ui, path, pps as usize, &stats);
            if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                ui.check_start("写入 postgres");
                let cfg_id = formal::insert_just_tx_config(
                    client,
                    exam_id,
                    *fw,
                    stats.target_pps,
                    window.as_secs_f64(),
                    stats.started_at,
                    stats.stopped_at,
                    stats.sent as i64,
                )
                .await?;
                let samples: Vec<TxSample> = stats
                    .samples
                    .iter()
                    .map(|s| TxSample {
                        t_tx: s.t_tx,
                        t_rx: s.t_rx,
                        got_rx: s.got_rx,
                    })
                    .collect();
                formal::insert_just_tx_data(client, cfg_id, &samples).await?;
                let msg = agent.request("report").await.context("report")?;
                let decoded = formal::decode_report_msg(&msg)?;
                formal::write_perf(client, exam_id, cfg_id, *fw, &decoded).await?;
                ui.check_ok(&format!(
                    "config={cfg_id} samples={} mask={:#x}",
                    samples.len(),
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

    // Trace arm: one packet.
    {
        ui.begin_round();
        ui.set_eta(&eta.fmt());
        let fw = Firmware::XdpT;
        let profile = profile_for(fw);
        let path = PpsPath::Xdp;
        ui.check_pending(&format!("烧录 `{profile}`"));
        ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
        ui.check_pending("DUT ready");
        flash_and_ready(cfg, &agent, &ui, &mut eta, path, spoof, profile).await?;
        agent.perf_clear().await.context("perf_clear")?;
        harness.reset_state().await;
        let started_at = SystemTime::now();
        let seq = harness.alloc_seq();
        let t_tx = SystemTime::now();
        let _ = harness.send_stimulus(spoof, None, seq).await;
        tokio::time::sleep(DRAIN).await;
        let sight = harness.take_seq(seq).await;
        let got_rx = sight.t0_ns.is_some()
            && sight.t_bounce_ns.or(sight.t_echo_ns).is_some();
        let stopped_at = SystemTime::now();
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 trace");
            let cfg_id = formal::insert_just_tx_config(
                client,
                exam_id,
                fw,
                1.0,
                0.0,
                started_at,
                stopped_at,
                1,
            )
            .await?;
            formal::insert_just_tx_data(
                client,
                cfg_id,
                &[TxSample {
                    t_tx,
                    t_rx: None,
                    got_rx,
                }],
            )
            .await?;
            let msg = agent.request("report").await.context("report")?;
            let decoded = formal::decode_report_msg(&msg)?;
            formal::write_perf(client, exam_id, cfg_id, fw, &decoded).await?;
            let n_ev = decoded.trace.as_ref().map(|t| t.events.len()).unwrap_or(0);
            ui.check_ok(&format!("trace events={n_ev}"));
        }
        ui.end_round();
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        formal::stop_exam(client, exam_id).await?;
    }

    RoundUi::banner(&format!("{name} done arms={} grid={}", arms.len(), grid.len()));
    Ok(())
}

async fn run_dev_flame(
    cfg: &FileConfig,
    _harness: &mut Harness,
    agent: &AgentClient,
    endpoint: &str,
    esp_s: &str,
) -> Result<()> {
    RoundUi::banner(&format!(
        "dev flame Trace XDP @{DEV_FLAME_PPS:.0}pps {}s → {DEV_FLAME_REPORT_PATH} agent={endpoint} esp={esp_s}",
        DEV_FLAME_WINDOW.as_secs()
    ));
    let spoof = crate::raw_tx::iface_ipv4(&cfg.probe.iface)
        .unwrap_or_else(|_| gen_lan_ip(cfg.target.esp_ip, &[cfg.target.esp_ip]));
    let mut eta = BudgetEta::new();
    eta.add_flashes(1);
    eta.add_fixed(READY_BUDGET + DEV_FLAME_WINDOW);
    let ui = RoundUi::new();
    ui.begin_round();
    let profile = "just-tx-t";
    flash_and_ready(cfg, agent, &ui, &mut eta, PpsPath::Xdp, spoof, profile).await?;
    agent.perf_clear().await.context("perf_clear")?;
    use crate::bench::just_drop_pps::paced_blast;
    let sent = paced_blast(cfg, &ui, &eta, PpsPath::Xdp, DEV_FLAME_WINDOW, DEV_FLAME_PPS).await?;
    tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
    let msg = agent.request("report").await.context("report")?;
    std::fs::write(DEV_FLAME_REPORT_PATH, msg.as_bytes())
        .with_context(|| format!("write {DEV_FLAME_REPORT_PATH}"))?;
    ui.permanent(&format!(
        "    flame report → {DEV_FLAME_REPORT_PATH} ({} bytes) sent={sent}",
        msg.len()
    ));
    ui.end_round();
    Ok(())
}


fn host_stall_thresh(pps: f64) -> Duration {
    let period = Duration::from_secs_f64(1.0 / pps.max(1.0));
    (period.saturating_mul(8))
        .max(Duration::from_millis(15))
        .min(Duration::from_millis(40))
}

#[derive(Clone, Copy)]
struct OpPoint {
    target_pps: f64,
    loss: f64,
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
    for i in 0..20 {
        match agent.request("ping").await {
            Ok(m) if m.contains("OK ping") => break,
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
    let _ = agent.perf_clear().await;
    let counters_before = counters::fetch_counters(agent).await?;
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
            t_tx: *t_tx,
            t_rx,
            got_rx,
        });
        if i % 32 == 0 || i + 1 == seqs.len() {
            ui.check_note(&format!("{ok_n}/{sent_n} ok"));
        }
    }
    ui.check_ok(&format!("{ok_n}/{sent_n} ok"));

    let after = agent.map_slot("rxcnt", IPPROTO_UDP).await?;
    let counters_after = counters::fetch_counters(agent).await?;
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
        n_t0,
        counters: counters::counters_delta(&counters_before, &counters_after),
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
        None => crate::raw_tx::iface_mac(&cfg.probe.iface)?,
    };
    let raw = std::sync::Arc::new(crate::raw_tx::RawTx::open(
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
