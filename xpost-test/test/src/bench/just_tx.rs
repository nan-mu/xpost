//! `just_tx` — per-packet stimulus / reply observation → `just_tx_config` / `just_tx_data`.
//!
//! Also drives hardcoded debug experiment `dev` → `dev_*` (XDP only; 240/480/720/1200;
//! 1 trial; no operating rounds; never DROPs `just_tx_*` / `just_drop_*`).
//!
//! Arms: `hal` (`just-tx-hal`) and `xdp` (`just-tx`). Shared send-rate sweep finds ~5% loss
//! operating point; every window (sweep + operating) is persisted as one config + packet rows.
//! Formal `just_tx`: DROP same-named tables then CREATE empty (`TableSet::JustTx`).

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;
use serde_json::Value as JsonValue;
use tokio_postgres::Client;

use crate::agent::AgentClient;
use crate::bench::counters;
use crate::bench::just_drop_pps::PpsPath;
use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::config::FileConfig;
use crate::harness::Harness;

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const PERF_WINDOW: Duration = Duration::from_secs(20);
const PERF_SMOKE_WINDOW: Duration = Duration::from_secs(5);
const SWEEP_WINDOW: Duration = Duration::from_secs(8);
const SWEEP_SMOKE_WINDOW: Duration = Duration::from_secs(4);
const WARMUP: Duration = Duration::from_secs(1);
const COOLDOWN: Duration = Duration::from_secs(2);
const DRAIN: Duration = Duration::from_millis(1000);
const TARGET_LOSS: f64 = 0.05;
const LOSS_LO: f64 = 0.04;
const LOSS_HI: f64 = 0.07;
const RX_READY_TRIES: usize = 40;
const RX_READY_BURST: u32 = 64;

/// Shared TR3000 send-rate grid (pps) for HAL and XDP.
const SWEEP_PPS: &[f64] = &[
    80.0, 160.0, 240.0, 320.0, 400.0, 480.0, 560.0, 720.0, 960.0, 1200.0,
];
const SWEEP_SMOKE_PPS: &[f64] = &[160.0, 320.0, 480.0, 720.0];
/// Hardcoded `dev` grid — XDP TX debug only.
const DEV_PPS: &[f64] = &[240.0, 480.0, 720.0, 1200.0];
const DEV_SWEEP_WINDOW: Duration = Duration::from_secs(4);

pub struct JustTxOpts {
    pub paths: Vec<PpsPath>,
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub rounds: Option<usize>,
    /// `JustTx` (formal) or `Dev` (hardcoded debug sink).
    pub tables: TableSet,
}

fn exp_name(tables: TableSet) -> &'static str {
    match tables {
        TableSet::Dev => "dev",
        _ => "just_tx",
    }
}

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
    /// Counter delta for this window only (changed keys).
    counters: JsonValue,
}

impl WindowStats {
    fn t0_frac(&self) -> f64 {
        if self.sent == 0 {
            0.0
        } else {
            self.n_t0 as f64 / self.sent as f64
        }
    }
}

#[derive(Clone, Copy)]
struct OpPoint {
    target_pps: f64,
    loss: f64,
}

pub async fn run_just_tx(cfg: &FileConfig, harness: &mut Harness, opts: JustTxOpts) -> Result<()> {
    let name = exp_name(opts.tables);
    let is_dev = matches!(opts.tables, TableSet::Dev);
    // `dev` is always append-only — never honor a formal DROP of just_tx_*.
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

    let paths = if is_dev {
        vec![PpsPath::Xdp]
    } else if opts.paths.is_empty() {
        cfg.just_tx
            .arms
            .iter()
            .map(|s| PpsPath::parse(s))
            .collect::<Result<Vec<_>>>()?
    } else {
        opts.paths
    };
    if paths.is_empty() {
        bail!("{name}: no arms");
    }

    let (grid, sweep_window, sweep_trials, rounds, op_window) = if is_dev {
        // Hardcoded: 4 rates × 1 trial × 4s window; no operating rounds.
        (
            DEV_PPS,
            DEV_SWEEP_WINDOW,
            1usize,
            0usize,
            Duration::from_secs(0),
        )
    } else {
        let op_window = opts
            .window_secs
            .map(Duration::from_secs)
            .or_else(|| cfg.just_tx.window_secs.map(Duration::from_secs))
            .unwrap_or(if opts.smoke {
                PERF_SMOKE_WINDOW
            } else {
                PERF_WINDOW
            });
        let sweep_window = if opts.smoke {
            SWEEP_SMOKE_WINDOW
        } else {
            SWEEP_WINDOW
        };
        let sweep_trials = if opts.smoke { 1 } else { 3 };
        let rounds = opts.rounds.unwrap_or(if opts.smoke {
            1
        } else {
            cfg.just_tx.rounds.max(1)
        });
        let grid: &[f64] = if opts.smoke {
            SWEEP_SMOKE_PPS
        } else {
            SWEEP_PPS
        };
        (grid, sweep_window, sweep_trials, rounds, op_window)
    };

    RoundUi::banner(&format!(
        "{name} arms={:?} tables={:?} agent={endpoint} esp={esp_s} sweep_grid={:?} sweep_trials={sweep_trials} sweep_win={}s op_win={}s op_rounds={rounds} target_loss={:.0}% formal={formal} pg={}",
        paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        opts.tables,
        grid.iter().map(|x| *x as i64).collect::<Vec<_>>(),
        sweep_window.as_secs(),
        op_window.as_secs(),
        100.0 * TARGET_LOSS,
        !(is_dev || opts.no_pg)
    ));

    // `dev` is console-only — never connect Postgres / touch just_tx_* tables.
    let mut pg = if is_dev || opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts { formal };
        Some(pg_dsn::connect(pg_opts).await?)
    };

    let spoof = crate::raw_tx::iface_ipv4(&cfg.probe.iface)
        .unwrap_or_else(|_| gen_lan_ip(cfg.target.esp_ip, &[cfg.target.esp_ip]));
    RoundUi::banner(&format!(
        "  stim_src={spoof} (iface {} IPv4 preferred for HAL ARP)",
        cfg.probe.iface
    ));

    let sweep_unit = WARMUP + sweep_window + DRAIN + COOLDOWN;
    let op_unit = WARMUP + op_window + DRAIN;
    let per_arm_fixed = READY_BUDGET
        + sweep_unit.saturating_mul((grid.len() * sweep_trials) as u32)
        + op_unit.saturating_mul(rounds as u32);

    let mut eta = BudgetEta::new();
    eta.add_flashes(paths.len());
    eta.add_fixed(per_arm_fixed.saturating_mul(paths.len() as u32));

    let ui = RoundUi::new();
    let mut first_round = true;

    for path in &paths {
        let (op, writes) = run_arm_round(
            cfg,
            &agent,
            harness,
            &ui,
            &mut eta,
            *path,
            spoof,
            grid,
            sweep_trials,
            sweep_window,
            op_window,
            rounds,
            formal,
            opts.tables,
            first_round,
            pg.as_mut(),
        )
        .await?;
        first_round = false;
        RoundUi::banner(&format!(
            "  arm {}: operating done target_pps={:.0} sweep_loss_med={:.2}% configs={writes}",
            path.as_str(),
            op.target_pps,
            100.0 * op.loss,
        ));
    }

    RoundUi::banner(&format!(
        "{} done arms={} op_rounds={rounds}",
        exp_name(opts.tables),
        paths.len()
    ));
    Ok(())
}

async fn run_arm_round(
    cfg: &FileConfig,
    agent: &AgentClient,
    harness: &mut Harness,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    spoof: Ipv4Addr,
    grid: &[f64],
    trials: usize,
    sweep_window: Duration,
    op_window: Duration,
    op_rounds: usize,
    formal: bool,
    tables: TableSet,
    first_round: bool,
    mut pg: Option<&mut Client>,
) -> Result<(OpPoint, u64)> {
    ui.begin_round();
    ui.set_eta(&eta.fmt());

    if first_round {
        if let Some(client) = pg.as_ref() {
            let label = match tables {
                TableSet::Dev => "ensure dev_*",
                _ => "DROP+CREATE just_tx_*",
            };
            ui.check_start(label);
            let pg_opts = PgOpts { formal };
            pg_dsn::prepare(client, pg_opts, tables)
                .await
                .context("prepare tx tables")?;
            ui.check_ok(if formal && !matches!(tables, TableSet::Dev) {
                "recreated"
            } else {
                "ok"
            });
        }
    }

    let profile = match path {
        PpsPath::Hal => cfg
            .profiles
            .just_tx_hal
            .as_deref()
            .unwrap_or("just-tx-hal"),
        PpsPath::Xdp => cfg.profiles.just_tx.as_deref().unwrap_or("just-tx"),
    };
    ui.check_pending(&format!("烧录 `{profile}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.check_pending("DUT ready");

    flash_and_ready(cfg, agent, ui, eta, path, spoof).await?;

    let mut writes = 0u64;
    let mut medians: Vec<(f64, f64)> = Vec::new();
    let is_dev = matches!(tables, TableSet::Dev);
    for &pps in grid {
        let mut losses = Vec::with_capacity(trials);
        for trial in 1..=trials {
            let stats = match probe_window(
                harness,
                agent,
                ui,
                eta,
                path,
                spoof,
                pps,
                sweep_window,
            )
            .await
            {
                Ok(mut stats) => {
                    if stats.sent > 0 && stats.t0_frac() < 0.5 {
                        ui.permanent(&format!(
                            "    sweep_retry_t0: pps={pps:.0} trial={trial} t0_frac={:.2}",
                            stats.t0_frac()
                        ));
                        harness.reset_state().await;
                        tokio::time::sleep(COOLDOWN).await;
                        eta.add_fixed(WARMUP + sweep_window + DRAIN + COOLDOWN);
                        ui.set_eta(&eta.fmt());
                        stats = probe_window(
                            harness,
                            agent,
                            ui,
                            eta,
                            path,
                            spoof,
                            pps,
                            sweep_window,
                        )
                        .await?;
                    }
                    stats
                }
                Err(e) if is_dev => {
                    // High-rate XDP_TX can hang the DUT; reflash and continue next rate.
                    ui.permanent(&format!(
                        "    sweep {} pps={pps:.0} trial={trial}/{trials}: FAIL {e:#} → reflash",
                        path.as_str(),
                    ));
                    flash_and_ready(cfg, agent, ui, eta, path, spoof).await?;
                    losses.push(1.0);
                    harness.reset_state().await;
                    tokio::time::sleep(COOLDOWN).await;
                    eta.consume_fixed(WARMUP + sweep_window + DRAIN + COOLDOWN);
                    ui.set_eta(&eta.fmt());
                    continue;
                }
                Err(e) => return Err(e),
            };
            ui.permanent(&format!(
                "    sweep {} pps={pps:.0} trial={trial}/{trials}: sent={} ok={} loss={:.2}%",
                path.as_str(),
                stats.sent,
                stats.ok,
                100.0 * stats.loss,
            ));
            if is_dev {
                ui.permanent(&format!("      countersΔ {}", stats.counters));
                match agent.request("txstats").await {
                    Ok(line) => ui.permanent(&format!("      {line}")),
                    Err(e) => ui.permanent(&format!("      txstats FAIL {e:#}")),
                }
                let until = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let since = until.saturating_sub(sweep_window.as_millis() as u64 + 5_000);
                match agent.serial_window(since, until).await {
                    Ok(msg) => {
                        for line in msg.lines().filter(|l| l.contains("tsflow")) {
                            ui.permanent(&format!("      {line}"));
                        }
                    }
                    Err(e) => ui.permanent(&format!("      serial_window FAIL {e:#}")),
                }
                let _ = agent.request("tx clear").await;
            }
            losses.push(stats.loss);
            if let Some(client) = pg.as_mut() {
                push_config(client, ui, tables, path, &stats).await?;
                writes += 1;
            }
            harness.reset_state().await;
            tokio::time::sleep(COOLDOWN).await;
            eta.consume_fixed(WARMUP + sweep_window + DRAIN + COOLDOWN);
            ui.set_eta(&eta.fmt());
        }
        let med = median_f64(&mut losses).unwrap_or(1.0);
        medians.push((pps, med));
        ui.permanent(&format!(
            "    sweep {} pps={pps:.0} loss_med={:.2}%",
            path.as_str(),
            100.0 * med
        ));
    }

    let op = pick_operating_point(&medians);
    if op_rounds > 0 {
        ui.permanent(&format!(
            "  arm {}: operating target_pps={:.0} sweep_loss_med={:.2}% (band {:.0}%–{:.0}%)",
            path.as_str(),
            op.target_pps,
            100.0 * op.loss,
            100.0 * LOSS_LO,
            100.0 * LOSS_HI
        ));
    }

    for pair_id in 1..=op_rounds {
        if pair_id > 1 {
            ensure_alive(cfg, agent, ui, eta, path, spoof).await?;
        }
        let stats =
            probe_window(harness, agent, ui, eta, path, spoof, op.target_pps, op_window).await?;
        log_window(ui, path, pair_id, &stats);
        if let Some(client) = pg.as_mut() {
            push_config(client, ui, tables, path, &stats).await?;
            writes += 1;
        }
        harness.reset_state().await;
        eta.consume_fixed(WARMUP + op_window + DRAIN);
        ui.set_eta(&eta.fmt());
    }

    ui.end_round();
    Ok((op, writes))
}

/// Finished window → push one config row + its packet rows immediately.
async fn push_config(
    client: &Client,
    ui: &RoundUi,
    tables: TableSet,
    path: PpsPath,
    stats: &WindowStats,
) -> Result<()> {
    ui.check_start(&format!(
        "push config @{:.0}pps sent={}",
        stats.target_pps, stats.sent
    ));
    ingest_window(client, tables, path, stats).await?;
    ui.check_ok(&format!("{} pkts", stats.samples.len()));
    Ok(())
}

fn pick_operating_point(medians: &[(f64, f64)]) -> OpPoint {
    let mut best: Option<OpPoint> = None;
    for &(pps, loss) in medians {
        let cand = OpPoint {
            target_pps: pps,
            loss,
        };
        let better = match best {
            None => true,
            Some(b) => {
                let c_in = (LOSS_LO..=LOSS_HI).contains(&cand.loss);
                let b_in = (LOSS_LO..=LOSS_HI).contains(&b.loss);
                if c_in && b_in {
                    cand.target_pps > b.target_pps
                } else if c_in {
                    true
                } else if b_in {
                    false
                } else {
                    (cand.loss - TARGET_LOSS).abs() < (b.loss - TARGET_LOSS).abs()
                }
            }
        };
        if better {
            best = Some(cand);
        }
    }
    let op = best.unwrap_or(OpPoint {
        target_pps: medians.first().map(|x| x.0).unwrap_or(80.0),
        loss: 1.0,
    });
    if !(LOSS_LO..=LOSS_HI).contains(&op.loss) {
        eprintln!(
            "    WARN: operating loss {:.2}% outside [{:.0}%,{:.0}%]; closest to 5%",
            100.0 * op.loss,
            100.0 * LOSS_LO,
            100.0 * LOSS_HI
        );
    }
    op
}

fn log_window(ui: &RoundUi, path: PpsPath, pair_id: usize, stats: &WindowStats) {
    ui.permanent(&format!(
        "    {} pair={pair_id}: sent={} ok={} loss={:.2}% rxcntΔ={}",
        path.as_str(),
        stats.sent,
        stats.ok,
        100.0 * stats.loss,
        stats.device_rxcnt_delta
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

async fn ensure_alive(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    spoof: Ipv4Addr,
) -> Result<()> {
    for i in 0..10 {
        match agent.request("ping").await {
            Ok(m) if m.contains("OK ping") => return Ok(()),
            Ok(m) => ui.check_note(&format!("ping try{i}: {m}")),
            Err(e) => ui.check_note(&format!("ping try{i}: {e:#}")),
        }
        if i == 9 {
            ui.begin_round();
            ui.set_eta(&eta.fmt());
            eta.add_flashes(1);
            eta.add_fixed(READY_BUDGET);
            let profile = match path {
                PpsPath::Hal => cfg
                    .profiles
                    .just_tx_hal
                    .as_deref()
                    .unwrap_or("just-tx-hal"),
                PpsPath::Xdp => cfg.profiles.just_tx.as_deref().unwrap_or("just-tx"),
            };
            ui.check_pending(&format!("烧录 `{profile}`"));
            ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
            ui.check_pending("DUT ready");
            flash_and_ready(cfg, agent, ui, eta, path, spoof).await?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}

async fn flash_and_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    stim_src: Ipv4Addr,
) -> Result<()> {
    let profile = match path {
        PpsPath::Hal => cfg
            .profiles
            .just_tx_hal
            .as_deref()
            .unwrap_or("just-tx-hal"),
        PpsPath::Xdp => cfg.profiles.just_tx.as_deref().unwrap_or("just-tx"),
    };
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

    if !WARMUP.is_zero() {
        harness.reset_state().await;
        let warm_deadline = Instant::now() + WARMUP;
        let mut next = Instant::now();
        while Instant::now() < warm_deadline {
            let now = Instant::now();
            if now < next {
                tokio::time::sleep(next.saturating_duration_since(now)).await;
            }
            let seq = harness.alloc_seq();
            let _ = harness.send_stimulus(spoof, None, seq).await;
            next += interval;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        harness.reset_state().await;
    }

    let before = parse_rxcnt(&agent.request(&format!("rxcnt {IPPROTO_UDP}")).await?)?;
    let counters_before = counters::fetch_counters(agent).await?;
    harness.reset_state().await;
    let started_at = SystemTime::now();
    let start = Instant::now();
    let deadline = start + window;
    let mut next = start;
    let mut seqs: Vec<(u64, SystemTime)> = Vec::new();
    let mut last_paint = Instant::now() - Duration::from_secs(1);
    let expect = (pps * window.as_secs_f64()).max(1.0);

    while Instant::now() < deadline {
        let now = Instant::now();
        if now < next {
            tokio::time::sleep(next.saturating_duration_since(now)).await;
        }
        if Instant::now() >= deadline {
            break;
        }
        let seq = harness.alloc_seq();
        let t_wall = SystemTime::now();
        if harness.send_stimulus(spoof, None, seq).await.is_ok() {
            seqs.push((seq, t_wall));
        }
        next += interval;

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

    let after = parse_rxcnt(&agent.request(&format!("rxcnt {IPPROTO_UDP}")).await?)?;
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
    })
}

fn median_f64(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = (v.len() - 1) / 2;
    Some(v[idx])
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
        let before = parse_rxcnt(&agent.request(&format!("rxcnt {IPPROTO_UDP}")).await?)?;
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
        let after = parse_rxcnt(&agent.request(&format!("rxcnt {IPPROTO_UDP}")).await?)?;
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

async fn ingest_window(
    client: &Client,
    tables: TableSet,
    path: PpsPath,
    stats: &WindowStats,
) -> Result<()> {
    let (cfg_table, data_table) = match tables {
        TableSet::Dev => ("dev_config", "dev_data"),
        _ => ("just_tx_config", "just_tx_data"),
    };
    let insert_cfg = format!(
        r#"INSERT INTO {cfg_table} (
                fw_type, target_pps, started_at, stopped_at, sent, counters
            ) VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id"#
    );
    let row = client
        .query_one(
            &insert_cfg,
            &[
                &path.as_str(),
                &stats.target_pps,
                &stats.started_at,
                &stats.stopped_at,
                &(stats.sent as i64),
                &stats.counters,
            ],
        )
        .await
        .with_context(|| format!("INSERT {cfg_table}"))?;
    let config_id = pg_dsn::config_id_from_row(&row)?;

    let insert_data = format!(
        r#"INSERT INTO {data_table} (config_id, t_tx, t_rx, got_rx)
                   SELECT * FROM UNNEST($1::bigint[], $2::timestamptz[], $3::timestamptz[], $4::bool[])"#
    );
    for chunk in stats.samples.chunks(512) {
        let ids: Vec<i64> = chunk.iter().map(|_| config_id).collect();
        let t_txs: Vec<SystemTime> = chunk.iter().map(|s| s.t_tx).collect();
        let t_rxs: Vec<Option<SystemTime>> = chunk.iter().map(|s| s.t_rx).collect();
        let got: Vec<bool> = chunk.iter().map(|s| s.got_rx).collect();
        client
            .execute(&insert_data, &[&ids, &t_txs, &t_rxs, &got])
            .await
            .with_context(|| format!("INSERT {data_table}"))?;
    }
    Ok(())
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

fn parse_rxcnt(line: &str) -> Result<u64> {
    let line = line.trim();
    let body = line
        .strip_prefix("OK rxcnt ")
        .or_else(|| line.strip_prefix("OK RXCNT "))
        .context("missing OK rxcnt")?;
    let mut value = None;
    for part in body.split_whitespace() {
        if let Some(v) = part.strip_prefix("value=") {
            value = Some(v.parse().context("value=")?);
        }
    }
    value.context("missing value=")
}
