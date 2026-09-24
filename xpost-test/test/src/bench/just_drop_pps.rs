//! `probe just-drop-pps` — periodic `drop` sampling during UDP blast → dut_pps.
//!
//! Paths: `xdp` (cold-loaded just_drop ELF) vs `hal` (`drop on` on just_drop_hal).
//! Writes CSV time series and prints overall / interval PPS summaries.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rand::RngExt;

use crate::agent::AgentClient;
use crate::bench::udp_blast;
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
const DEFAULT_ELF: &str = "/Users/nan/bs/xpost/examples/just_drop/just_drop.elf";
const REBOOT_SETTLE: Duration = Duration::from_secs(20);
const PERF_WINDOW: Duration = Duration::from_secs(40);
const PERF_SMOKE_WINDOW: Duration = Duration::from_secs(10);
const DRAIN_MS: u64 = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PpsPath {
    Xdp,
    Hal,
}

impl PpsPath {
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "xdp" => Ok(Self::Xdp),
            "hal" => Ok(Self::Hal),
            other => bail!("--path must be xdp|hal, got {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xdp => "xdp",
            Self::Hal => "hal",
        }
    }
}

pub struct JustDropPpsOpts {
    pub path: PpsPath,
    pub smoke: bool,
    pub window_secs: Option<u64>,
    pub sample_interval_ms: u64,
    pub rounds: Option<usize>,
    pub out: PathBuf,
    /// When true (default for xdp), program the drop ELF before rounds.
    pub program: bool,
}

#[derive(Clone, Copy, Debug)]
struct Counters {
    drop: u64,
    net_rx: u64,
}

#[derive(Clone, Debug)]
struct Sample {
    t_s: f64,
    drop: u64,
    net_rx: u64,
}

#[derive(Clone, Debug)]
struct RoundSummary {
    round: usize,
    overall_pps: f64,
    interval_pps_mean: f64,
    interval_pps_std: f64,
    host_pps: f64,
    drop_delta: u64,
    net_rx_delta: u64,
    elapsed_secs: f64,
}

pub async fn run_just_drop_pps(cfg: &FileConfig, opts: JustDropPpsOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("just-drop-pps needs [agent].endpoint or XPOST_AGENT_ENDPOINT")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp = cfg.target.esp_ip;
    let esp_s = esp.to_string();

    let window = opts
        .window_secs
        .map(Duration::from_secs)
        .unwrap_or(if opts.smoke {
            PERF_SMOKE_WINDOW
        } else {
            PERF_WINDOW
        });
    let rounds = opts
        .rounds
        .unwrap_or(if opts.smoke { 1 } else { 3 });
    let sample = Duration::from_millis(opts.sample_interval_ms.max(100));

    eprintln!(
        "just-drop-pps path={} agent={endpoint} esp={esp_s} window={}s sample={}ms rounds={rounds} out={}",
        opts.path.as_str(),
        window.as_secs(),
        sample.as_millis(),
        opts.out.display()
    );

    match opts.path {
        PpsPath::Xdp if opts.program => {
            let elf = std::env::var("XPOST_JUST_DROP_ELF").unwrap_or_else(|_| DEFAULT_ELF.into());
            eprintln!("  arm xdp: program {elf}");
            let msg = agent.program(&elf).await.context("program")?;
            eprintln!("  program ok: {msg}");
            settle_after_reboot().await;
        }
        PpsPath::Xdp => {
            eprintln!("  arm xdp: skip program (assume drop ELF already leased)");
        }
        PpsPath::Hal => {
            eprintln!("  arm hal: request drop on");
            let msg = agent.request("drop on").await.context("drop on")?;
            eprintln!("  {msg}");
        }
    }

    // Ping readiness
    for i in 0..20 {
        match agent.request("ping").await {
            Ok(m) if m.contains("OK ping") => break,
            Ok(m) => eprintln!("  ping try{i}: {m}"),
            Err(e) => eprintln!("  ping try{i}: {e:#}"),
        }
        if i == 19 {
            bail!("DUT not responding to ping");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    if let Some(parent) = opts.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
    }
    let mut csv = BufWriter::new(
        File::create(&opts.out).with_context(|| format!("create {}", opts.out.display()))?,
    );
    writeln!(
        csv,
        "path,round,t_s,drop,net_rx,host_tx_end,interval_pps,overall_pps_so_far"
    )?;

    let mut summaries = Vec::with_capacity(rounds);
    for round in 1..=rounds {
        eprintln!("  —— round {round}/{rounds} ——");
        let summary = run_round(
            cfg,
            &agent,
            opts.path,
            round,
            window,
            sample,
            &mut csv,
        )
        .await
        .with_context(|| format!("round {round}"))?;
        eprintln!(
            "  round {round}: overall_pps={:.0} interval_mean±std={:.0}±{:.0} host_pps={:.0} dropΔ={} net_rxΔ={}",
            summary.overall_pps,
            summary.interval_pps_mean,
            summary.interval_pps_std,
            summary.host_pps,
            summary.drop_delta,
            summary.net_rx_delta
        );
        summaries.push(summary);
        if round < rounds {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    csv.flush()?;

    let overalls: Vec<f64> = summaries.iter().map(|s| s.overall_pps).collect();
    let (mean, std) = mean_std(&overalls);
    eprintln!(
        "just-drop-pps done path={} rounds={rounds} overall_pps mean±std={mean:.0}±{std:.0} csv={}",
        opts.path.as_str(),
        opts.out.display()
    );

    // Sidecar summary JSON next to CSV when possible.
    if let Some(sum_path) = summary_path_for(&opts.out) {
        let body = serde_json::json!({
            "path": opts.path.as_str(),
            "window_secs": window.as_secs(),
            "sample_interval_ms": sample.as_millis() as u64,
            "rounds": rounds,
            "overall_pps_mean": mean,
            "overall_pps_std": std,
            "rounds_detail": summaries.iter().map(|s| serde_json::json!({
                "round": s.round,
                "overall_pps": s.overall_pps,
                "interval_pps_mean": s.interval_pps_mean,
                "interval_pps_std": s.interval_pps_std,
                "host_pps": s.host_pps,
                "drop_delta": s.drop_delta,
                "net_rx_delta": s.net_rx_delta,
                "elapsed_secs": s.elapsed_secs,
            })).collect::<Vec<_>>(),
        });
        std::fs::write(&sum_path, serde_json::to_string_pretty(&body)?)
            .with_context(|| format!("write {}", sum_path.display()))?;
        eprintln!("  wrote {}", sum_path.display());
    }

    Ok(())
}

fn summary_path_for(csv: &Path) -> Option<PathBuf> {
    let stem = csv.file_stem()?.to_str()?;
    Some(csv.with_file_name(format!("{stem}_summary.json")))
}

async fn run_round(
    cfg: &FileConfig,
    agent: &AgentClient,
    path: PpsPath,
    round: usize,
    window: Duration,
    sample: Duration,
    csv: &mut BufWriter<File>,
) -> Result<RoundSummary> {
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
    let spoof = gen_lan_ip(esp, &[esp]);

    let t0 = Instant::now();
    let first = parse_counters(&agent.counters().await.context("counters t0")?)?;
    let mut samples = vec![Sample {
        t_s: 0.0,
        drop: first.drop,
        net_rx: first.net_rx,
    }];

    let blast_handle = tokio::task::spawn_blocking({
        let raw = Arc::new(raw);
        move || udp_blast::run_blast_window(raw, spoof, esp, STIM_PORT, window)
    });

    // Sample while blast runs; stop shortly after window.
    let deadline = t0 + window + Duration::from_millis(DRAIN_MS);
    while Instant::now() < deadline {
        tokio::time::sleep(sample).await;
        let line = agent.counters().await.context("counters sample")?;
        let c = parse_counters(&line)?;
        samples.push(Sample {
            t_s: t0.elapsed().as_secs_f64(),
            drop: c.drop,
            net_rx: c.net_rx,
        });
        if blast_handle.is_finished() {
            break;
        }
    }

    let blast = blast_handle
        .await
        .context("blast join")?
        .context("udp_blast")?;
    tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
    let last = parse_counters(&agent.counters().await.context("counters end")?)?;
    let t_end = t0.elapsed().as_secs_f64();
    if samples
        .last()
        .map(|s| (s.drop != last.drop) || (t_end - s.t_s) > 0.05)
        .unwrap_or(true)
    {
        samples.push(Sample {
            t_s: t_end,
            drop: last.drop,
            net_rx: last.net_rx,
        });
    }

    let drop_delta = last.drop.saturating_sub(first.drop);
    let net_rx_delta = last.net_rx.saturating_sub(first.net_rx);
    let elapsed = blast.elapsed_secs();
    let overall_pps = drop_delta as f64 / elapsed;
    let host_pps = blast.host_pps();

    if path == PpsPath::Xdp && net_rx_delta != 0 {
        bail!("xdp path: net_rx moved under drop ({net_rx_delta}); stack saw traffic");
    }
    if drop_delta == 0 {
        bail!("drop did not increase during blast");
    }

    let mut interval_pps = Vec::new();
    for i in 1..samples.len() {
        let dt = (samples[i].t_s - samples[i - 1].t_s).max(1e-6);
        let dd = samples[i].drop.saturating_sub(samples[i - 1].drop);
        interval_pps.push(dd as f64 / dt);
    }
    let (interval_mean, interval_std) = mean_std(&interval_pps);

    for (i, s) in samples.iter().enumerate() {
        let ipps = if i == 0 {
            0.0
        } else {
            interval_pps[i - 1]
        };
        let overall_so_far = if s.t_s > 1e-6 {
            s.drop.saturating_sub(first.drop) as f64 / s.t_s
        } else {
            0.0
        };
        writeln!(
            csv,
            "{},{},{:.6},{},{},{},{:.3},{:.3}",
            path.as_str(),
            round,
            s.t_s,
            s.drop,
            s.net_rx,
            if i + 1 == samples.len() {
                blast.tx_count
            } else {
                0
            },
            ipps,
            overall_so_far
        )?;
    }

    Ok(RoundSummary {
        round,
        overall_pps,
        interval_pps_mean: interval_mean,
        interval_pps_std: interval_std,
        host_pps,
        drop_delta,
        net_rx_delta,
        elapsed_secs: elapsed,
    })
}

async fn settle_after_reboot() {
    eprintln!("  settle {}s after DUT reset …", REBOOT_SETTLE.as_secs());
    tokio::time::sleep(REBOOT_SETTLE).await;
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

fn parse_counters(line: &str) -> Result<Counters> {
    let line = line.trim();
    let body = line
        .strip_prefix("OK counters ")
        .or_else(|| line.strip_prefix("OK COUNTERS "))
        .unwrap_or(line);
    let mut drop = None;
    let mut net_rx = None;
    for part in body.split_whitespace() {
        if let Some(v) = part.strip_prefix("drop=") {
            drop = Some(v.parse().context("drop=")?);
        } else if let Some(v) = part.strip_prefix("net_rx=") {
            net_rx = Some(v.parse().context("net_rx=")?);
        }
    }
    Ok(Counters {
        drop: drop.context("missing drop=")?,
        net_rx: net_rx.context("missing net_rx=")?,
    })
}

fn mean_std(xs: &[f64]) -> (f64, f64) {
    if xs.is_empty() {
        return (0.0, 0.0);
    }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    if xs.len() == 1 {
        return (mean, 0.0);
    }
    let var = xs.iter().map(|x| {
        let d = x - mean;
        d * d
    }).sum::<f64>() / (n - 1.0);
    (mean, var.sqrt())
}
