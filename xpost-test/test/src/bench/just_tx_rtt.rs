//! `xpost-test just-tx-rtt` — shared send-rate sweep (loss / return PPS) then RTT at ~5% loss.
//!
//! Arms: `hal` (`just-tx-hal`) and `xdp` (`just-tx`). Same `SWEEP_PPS` grid for both.
//! Formal writes `just_tx_rtt_*` with `phase` = `sweep` | `operating`; deletes only this
//! bench's `source_note` rows.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::agent::AgentClient;
use crate::bench::just_drop_pps::PpsPath;
use crate::bench::pg_dsn::{self, PgOpts};
use crate::config::FileConfig;
use crate::harness::Harness;

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const SOURCE_NOTE: &str = "xpost-test just-tx-rtt";
const REBOOT_SETTLE: Duration = Duration::from_secs(35);
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

const PHASE_SWEEP: &str = "sweep";
const PHASE_OPERATING: &str = "operating";

pub struct JustTxRttOpts {
    pub paths: Vec<PpsPath>,
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub rounds: Option<usize>,
}

#[derive(Clone, Debug)]
struct ProbeSample {
    t0: SystemTime,
    seq_i: i32,
    ok: bool,
    rtt_ms: Option<f64>,
}

#[derive(Clone, Debug)]
struct WindowStats {
    target_pps: f64,
    sent: u64,
    ok: u64,
    loss: f64,
    samples: Vec<ProbeSample>,
    device_rxcnt_delta: u64,
    median_rtt_ms: Option<f64>,
    p95_rtt_ms: Option<f64>,
    mean_rtt_ms: Option<f64>,
    return_pps: f64,
    n_t0: u64,
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

pub async fn run_just_tx_rtt(
    cfg: &FileConfig,
    harness: &mut Harness,
    opts: JustTxRttOpts,
) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("just-tx-rtt needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();

    let paths = if opts.paths.is_empty() {
        cfg.just_tx_rtt
            .arms
            .iter()
            .map(|s| PpsPath::parse(s))
            .collect::<Result<Vec<_>>>()?
    } else {
        opts.paths
    };
    if paths.is_empty() {
        bail!("just-tx-rtt: no arms (set [just_tx_rtt].arms or --path)");
    }

    let op_window = opts
        .window_secs
        .map(Duration::from_secs)
        .or_else(|| cfg.just_tx_rtt.window_secs.map(Duration::from_secs))
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
        cfg.just_tx_rtt.rounds.max(1)
    });
    let grid: &[f64] = if opts.smoke {
        SWEEP_SMOKE_PPS
    } else {
        SWEEP_PPS
    };
    let formal = opts.formal || cfg.just_tx_rtt.formal;

    eprintln!(
        "just-tx-rtt arms={:?} agent={endpoint} esp={esp_s} sweep_grid={:?} sweep_trials={sweep_trials} sweep_win={}s op_win={}s op_pairs={rounds} target_loss={:.0}% formal={formal} pg={}",
        paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        grid.iter().map(|x| *x as i64).collect::<Vec<_>>(),
        sweep_window.as_secs(),
        op_window.as_secs(),
        100.0 * TARGET_LOSS,
        !opts.no_pg
    );

    let mut pg = if opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts { formal };
        let client = pg_dsn::connect(pg_opts).await?;
        pg_dsn::prepare_just_tx_rtt_note(&client, pg_opts, SOURCE_NOTE).await?;
        Some(client)
    };

    let spoof = crate::raw_tx::iface_ipv4(&cfg.probe.iface)
        .unwrap_or_else(|_| gen_lan_ip(cfg.target.esp_ip, &[cfg.target.esp_ip]));
    eprintln!(
        "  stim_src={spoof} (iface {} IPv4 preferred for HAL ARP)",
        cfg.probe.iface
    );

    for path in &paths {
        let op = sweep_arm(
            cfg,
            &agent,
            harness,
            *path,
            spoof,
            grid,
            sweep_trials,
            sweep_window,
            pg.as_mut(),
        )
        .await?;
        eprintln!(
            "  arm {}: operating target_pps={:.0} sweep_loss_med={:.2}% (band {:.0}%–{:.0}%)",
            path.as_str(),
            op.target_pps,
            100.0 * op.loss,
            100.0 * LOSS_LO,
            100.0 * LOSS_HI
        );

        for pair_id in 1..=rounds {
            eprintln!(
                "  —— {} operating pair {pair_id}/{rounds} @ {:.0} pps ——",
                path.as_str(),
                op.target_pps
            );
            if pair_id > 1 {
                ensure_alive(cfg, &agent, *path, spoof).await?;
            }
            let stats =
                probe_window(harness, &agent, *path, spoof, op.target_pps, op_window).await?;
            log_window(*path, pair_id, &stats);
            if let Some(client) = pg.as_mut() {
                ingest_run(
                    client,
                    *path,
                    pair_id as i32,
                    PHASE_OPERATING,
                    op.target_pps,
                    op.loss,
                    op_window,
                    &stats,
                    /*write_samples*/ true,
                )
                .await?;
            }
            harness.reset_state().await;
        }
    }

    eprintln!("just-tx-rtt done arms={} op_pairs={rounds}", paths.len());
    Ok(())
}

async fn sweep_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    harness: &mut Harness,
    path: PpsPath,
    spoof: Ipv4Addr,
    grid: &[f64],
    trials: usize,
    window: Duration,
    mut pg: Option<&mut Client>,
) -> Result<OpPoint> {
    flash_and_ready(cfg, agent, path, spoof).await?;

    let mut medians: Vec<(f64, f64)> = Vec::new(); // (pps, median_loss)
    for &pps in grid {
        let mut losses = Vec::with_capacity(trials);
        for trial in 1..=trials {
            let mut stats = probe_window(harness, agent, path, spoof, pps, window).await?;
            if stats.sent > 0 && stats.t0_frac() < 0.5 {
                eprintln!(
                    "    sweep_retry_t0: pps={pps:.0} trial={trial} t0_frac={:.2}",
                    stats.t0_frac()
                );
                harness.reset_state().await;
                tokio::time::sleep(COOLDOWN).await;
                stats = probe_window(harness, agent, path, spoof, pps, window).await?;
            }
            eprintln!(
                "    sweep {} pps={pps:.0} trial={trial}/{trials}: sent={} ok={} loss={:.2}% return_pps={:.0} median_rtt={:?}",
                path.as_str(),
                stats.sent,
                stats.ok,
                100.0 * stats.loss,
                stats.return_pps,
                stats.median_rtt_ms
            );
            losses.push(stats.loss);
            if let Some(client) = pg.as_mut() {
                ingest_run(
                    client,
                    path,
                    trial as i32,
                    PHASE_SWEEP,
                    pps,
                    stats.loss,
                    window,
                    &stats,
                    /*write_samples*/ false,
                )
                .await?;
            }
            harness.reset_state().await;
            tokio::time::sleep(COOLDOWN).await;
        }
        let med = median_f64(&mut losses).unwrap_or(1.0);
        medians.push((pps, med));
        eprintln!(
            "    sweep {} pps={pps:.0} loss_med={:.2}%",
            path.as_str(),
            100.0 * med
        );
    }

    Ok(pick_operating_point(&medians))
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

fn log_window(path: PpsPath, pair_id: usize, stats: &WindowStats) {
    eprintln!(
        "    {} pair={pair_id}: sent={} ok={} loss={:.2}% return_pps={:.0} median_rtt_ms={:?} p95={:?} rxcntΔ={}",
        path.as_str(),
        stats.sent,
        stats.ok,
        100.0 * stats.loss,
        stats.return_pps,
        stats.median_rtt_ms,
        stats.p95_rtt_ms,
        stats.device_rxcnt_delta
    );
    let slack = (stats.sent / 50).max(8);
    if stats.device_rxcnt_delta + 2 < stats.ok {
        eprintln!(
            "    WARN: device rxcntΔ={} < host ok={}",
            stats.device_rxcnt_delta, stats.ok
        );
    } else if stats.device_rxcnt_delta > stats.ok + slack {
        eprintln!(
            "    WARN: device rxcntΔ={} ≫ host ok={}",
            stats.device_rxcnt_delta, stats.ok
        );
    }
}

async fn ensure_alive(
    cfg: &FileConfig,
    agent: &AgentClient,
    path: PpsPath,
    spoof: Ipv4Addr,
) -> Result<()> {
    for i in 0..10 {
        match agent.request("ping").await {
            Ok(m) if m.contains("OK ping") => return Ok(()),
            Ok(m) => eprintln!("    ping try{i}: {m}"),
            Err(e) => eprintln!("    ping try{i}: {e:#}"),
        }
        if i == 9 {
            flash_and_ready(cfg, agent, path, spoof).await?;
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}

async fn flash_and_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
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
    eprintln!("  arm {} flash profile `{profile}` …", path.as_str());
    agent
        .flash_profile(profile)
        .await
        .with_context(|| format!("FlashProfile `{profile}`"))?;
    agent
        .wait_idle(FLASH_IDLE)
        .await
        .context("wait agent idle after flash")?;
    tokio::time::sleep(REBOOT_SETTLE).await;

    for i in 0..20 {
        match agent.request("ping").await {
            Ok(m) if m.contains("OK ping") => break,
            Ok(m) => eprintln!("    ping try{i}: {m}"),
            Err(e) => eprintln!("    ping try{i}: {e:#}"),
        }
        if i == 19 {
            bail!("DUT not responding to ping after flash `{profile}`");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    wait_rx_ready(cfg, agent, path, stim_src).await
}

async fn probe_window(
    harness: &mut Harness,
    agent: &AgentClient,
    path: PpsPath,
    spoof: Ipv4Addr,
    target_pps: f64,
    window: Duration,
) -> Result<WindowStats> {
    let _ = path;
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
    harness.reset_state().await;
    let start = Instant::now();
    let deadline = start + window;
    let mut next = start;
    let mut seqs: Vec<(u64, SystemTime, i32)> = Vec::new();
    let mut seq_i = 0i32;

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
            seqs.push((seq, t_wall, seq_i));
            seq_i += 1;
        }
        next += interval;
    }

    tokio::time::sleep(DRAIN).await;

    let mut samples = Vec::with_capacity(seqs.len());
    let mut ok_n = 0u64;
    let mut rtts = Vec::new();
    let mut n_t0 = 0u64;
    let mut n_t1 = 0u64;
    for (seq, t_wall, idx) in &seqs {
        let sight = harness.take_seq(*seq).await;
        let t0 = sight.t0_ns;
        let t1 = sight.t_bounce_ns.or(sight.t_echo_ns);
        if t0.is_some() {
            n_t0 += 1;
        }
        if t1.is_some() {
            n_t1 += 1;
        }
        let (ok, rtt_ms) = match (t0, t1) {
            (Some(a), Some(b)) if b >= a => {
                let ms = (b - a) as f64 / 1_000_000.0;
                (true, Some(ms))
            }
            _ => (false, None),
        };
        if ok {
            ok_n += 1;
            if let Some(ms) = rtt_ms {
                rtts.push(ms);
            }
        }
        samples.push(ProbeSample {
            t0: *t_wall,
            seq_i: *idx,
            ok,
            rtt_ms,
        });
    }

    let after = parse_rxcnt(&agent.request(&format!("rxcnt {IPPROTO_UDP}")).await?)?;
    let sent = seqs.len() as u64;
    let loss = if sent == 0 {
        1.0
    } else {
        1.0 - (ok_n as f64 / sent as f64)
    };
    let elapsed = start.elapsed().as_secs_f64().max(1e-6);
    if ok_n == 0 && sent > 0 {
        eprintln!(
            "    diag: sent={sent} t0={n_t0} t1={n_t1} rxcntΔ={}",
            after.saturating_sub(before)
        );
    }
    Ok(WindowStats {
        target_pps: pps,
        sent,
        ok: ok_n,
        loss,
        samples,
        device_rxcnt_delta: after.saturating_sub(before),
        median_rtt_ms: percentile(&mut rtts, 0.50),
        p95_rtt_ms: percentile(&mut rtts.clone(), 0.95),
        mean_rtt_ms: if rtts.is_empty() {
            None
        } else {
            Some(rtts.iter().sum::<f64>() / rtts.len() as f64)
        },
        return_pps: ok_n as f64 / elapsed,
        n_t0,
    })
}

fn percentile(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
    Some(v[idx.min(v.len() - 1)])
}

fn median_f64(v: &mut [f64]) -> Option<f64> {
    percentile(v, 0.50)
}

async fn wait_rx_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
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
            eprintln!("    RX ready try{i}: rxcnt {before}→{after}");
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(());
        }
    }
    bail!("DUT never counted RX after flash ({})", path.as_str());
}

async fn ingest_run(
    client: &Client,
    path: PpsPath,
    pair_id: i32,
    phase: &str,
    target_pps: f64,
    cal_loss: f64,
    window: Duration,
    stats: &WindowStats,
    write_samples: bool,
) -> Result<()> {
    let run_id = Uuid::new_v4();
    let started = stats
        .samples
        .first()
        .map(|s| s.t0)
        .unwrap_or_else(SystemTime::now);
    let stopped = SystemTime::now();
    client
        .execute(
            r#"INSERT INTO just_tx_rtt_runs (
                run_id, pair_id, run_kind, variant, phase,
                target_pps, calibrated_loss, window_secs,
                sent, ok, loss_rate, return_pps,
                device_rxcnt_delta, median_rtt_ms, p95_rtt_ms, mean_rtt_ms,
                started_at_utc, stopped_at_utc, status, source_note
            ) VALUES (
                $1,$2,'pilot',$3,$4,
                $5,$6,$7,
                $8,$9,$10,$11,
                $12,$13,$14,$15,
                $16,$17,'complete',$18
            )"#,
            &[
                &run_id,
                &pair_id,
                &path.as_str(),
                &phase,
                &target_pps,
                &cal_loss,
                &(window.as_secs_f64()),
                &(stats.sent as i64),
                &(stats.ok as i64),
                &stats.loss,
                &stats.return_pps,
                &(stats.device_rxcnt_delta as i64),
                &stats.median_rtt_ms,
                &stats.p95_rtt_ms,
                &stats.mean_rtt_ms,
                &started,
                &stopped,
                &SOURCE_NOTE,
            ],
        )
        .await
        .context("INSERT just_tx_rtt_runs")?;

    if write_samples {
        for chunk in stats.samples.chunks(512) {
            let ids: Vec<Uuid> = chunk.iter().map(|_| run_id).collect();
            let seqs: Vec<i32> = chunk.iter().map(|s| s.seq_i).collect();
            let t0s: Vec<SystemTime> = chunk.iter().map(|s| s.t0).collect();
            let oks: Vec<bool> = chunk.iter().map(|s| s.ok).collect();
            let rtts: Vec<Option<f64>> = chunk.iter().map(|s| s.rtt_ms).collect();
            client
                .execute(
                    r#"INSERT INTO just_tx_rtt_samples (run_id, seq, t0, ok, rtt_ms)
                       SELECT * FROM UNNEST($1::uuid[], $2::int[], $3::timestamptz[], $4::bool[], $5::float8[])"#,
                    &[&ids, &seqs, &t0s, &oks, &rtts],
                )
                .await
                .context("INSERT just_tx_rtt_samples")?;
        }
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
