//! `xpost-test just-tx-pps` — flash profile per arm, sample `rxcnt 17` during UDP blast → `experiment_*`.
//!
//! Arms: `hal` (`just-tx-hal`) and `xdp` (`just-tx` = just_drop firmware + cold `just_tx.elf`).
//! Formal: schema `formal`, ensure tables, delete only this bench's `source_note` rows.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rand::RngExt;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::agent::AgentClient;
use crate::bench::just_drop_pps::PpsPath;
use crate::bench::pg_dsn::{self, PgOpts};
use crate::bench::udp_blast;
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const SOURCE_NOTE: &str = "xpost-test just-tx-pps";
const REBOOT_SETTLE: Duration = Duration::from_secs(25);
const FLASH_IDLE: Duration = Duration::from_secs(300);
const PERF_WINDOW: Duration = Duration::from_secs(40);
const PERF_SMOKE_WINDOW: Duration = Duration::from_secs(10);
const DRAIN_MS: u64 = 200;

pub struct JustTxPpsOpts {
    /// Arms to run (order preserved). Empty → from embedded `[just_tx_pps].arms`.
    pub paths: Vec<PpsPath>,
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub sample_interval_ms: u64,
    pub rounds: Option<usize>,
}

#[derive(Clone, Copy, Debug)]
struct Snapshot {
    rxcnt: u64,
    net_rx: u64,
}

#[derive(Clone, Debug)]
struct Sample {
    t_ns: i64,
    rxcnt: u64,
}

pub async fn run_just_tx_pps(cfg: &FileConfig, opts: JustTxPpsOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("just-tx-pps needs [agent].endpoint or XPOST_AGENT_ENDPOINT")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();

    let paths = if opts.paths.is_empty() {
        cfg.just_tx_pps
            .arms
            .iter()
            .map(|s| PpsPath::parse(s))
            .collect::<Result<Vec<_>>>()?
    } else {
        opts.paths
    };
    if paths.is_empty() {
        bail!("just-tx-pps: no arms (set [just_tx_pps].arms or --path)");
    }

    let window = opts
        .window_secs
        .map(Duration::from_secs)
        .or_else(|| cfg.just_tx_pps.window_secs.map(Duration::from_secs))
        .unwrap_or(if opts.smoke {
            PERF_SMOKE_WINDOW
        } else {
            PERF_WINDOW
        });
    let rounds = opts.rounds.unwrap_or(if opts.smoke {
        1
    } else {
        cfg.just_tx_pps.rounds.max(1)
    });
    let sample = Duration::from_millis(opts.sample_interval_ms.max(100));
    let formal = opts.formal || cfg.just_tx_pps.formal;
    let run_kind = "pilot";

    eprintln!(
        "just-tx-pps arms={:?} agent={endpoint} esp={esp_s} window={}s sample={}ms pairs={rounds} formal={formal} pg={}",
        paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        window.as_secs(),
        sample.as_millis(),
        !opts.no_pg
    );

    let mut pg = if opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts { formal };
        let client = pg_dsn::connect(pg_opts).await?;
        pg_dsn::prepare_experiment_pps_note(&client, pg_opts, SOURCE_NOTE).await?;
        Some(client)
    };

    for pair_id in 1..=rounds {
        eprintln!("  —— pair {pair_id}/{rounds} ——");
        for path in &paths {
            run_arm(
                cfg,
                &agent,
                *path,
                pair_id as i32,
                window,
                sample,
                run_kind,
                pg.as_mut(),
            )
            .await
            .with_context(|| format!("pair={pair_id} path={}", path.as_str()))?;
        }
    }

    eprintln!("just-tx-pps done pairs={rounds} arms={}", paths.len());
    Ok(())
}

async fn run_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    path: PpsPath,
    pair_id: i32,
    window: Duration,
    sample: Duration,
    run_kind: &str,
    pg: Option<&mut Client>,
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
    settle_after_reboot().await;

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

    let run_id = Uuid::new_v4();
    let (samples, rxcnt_delta, net_rx_delta, host_pps, elapsed) =
        measure_round(cfg, agent, path, window, sample).await?;

    eprintln!(
        "    {} pair={pair_id}: rxcntΔ={rxcnt_delta} net_rxΔ={net_rx_delta} host_pps={host_pps:.0} elapsed={elapsed:.2}s samples={}",
        path.as_str(),
        samples.len()
    );

    if let Some(client) = pg {
        ingest_run(
            client,
            run_id,
            pair_id,
            run_kind,
            path,
            window,
            sample,
            &samples,
            rxcnt_delta,
            host_pps,
        )
        .await?;
    }

    Ok(())
}

async fn measure_round(
    cfg: &FileConfig,
    agent: &AgentClient,
    path: PpsPath,
    window: Duration,
    sample: Duration,
) -> Result<(Vec<Sample>, u64, u64, f64, f64)> {
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
    let first = read_snapshot(agent).await.context("snapshot t0")?;
    let mut samples = vec![Sample {
        t_ns: 0,
        rxcnt: first.rxcnt,
    }];

    let blast_handle = tokio::task::spawn_blocking({
        let raw = Arc::new(raw);
        move || udp_blast::run_blast_window(raw, spoof, esp, STIM_PORT, window)
    });

    let deadline = t0 + window + Duration::from_millis(DRAIN_MS);
    while Instant::now() < deadline {
        tokio::time::sleep(sample).await;
        let s = read_snapshot(agent).await.context("snapshot sample")?;
        samples.push(Sample {
            t_ns: t0.elapsed().as_nanos() as i64,
            rxcnt: s.rxcnt,
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
    let last = read_snapshot(agent).await.context("snapshot end")?;
    let t_end = t0.elapsed().as_nanos() as i64;
    if samples
        .last()
        .map(|s| (s.rxcnt != last.rxcnt) || (t_end - s.t_ns) > 50_000_000)
        .unwrap_or(true)
    {
        samples.push(Sample {
            t_ns: t_end,
            rxcnt: last.rxcnt,
        });
    }

    if last.rxcnt < first.rxcnt {
        bail!(
            "rxcnt decreased {} → {} (reboot/wrap?)",
            first.rxcnt,
            last.rxcnt
        );
    }
    let rxcnt_delta = last.rxcnt - first.rxcnt;
    let net_rx_delta = last.net_rx.saturating_sub(first.net_rx);
    let elapsed = blast.elapsed_secs();
    let host_pps = blast.host_pps();

    if path == PpsPath::Xdp && net_rx_delta != 0 {
        bail!("xdp path: net_rx moved under just_tx ({net_rx_delta}); stack saw traffic");
    }
    if rxcnt_delta == 0 {
        bail!("rxcnt 17 did not increase during blast");
    }

    Ok((samples, rxcnt_delta, net_rx_delta, host_pps, elapsed))
}

async fn ingest_run(
    client: &mut Client,
    run_id: Uuid,
    pair_id: i32,
    run_kind: &str,
    path: PpsPath,
    window: Duration,
    sample: Duration,
    samples: &[Sample],
    rxcnt_delta: u64,
    host_pps: f64,
) -> Result<()> {
    client
        .execute(
            r#"
            INSERT INTO experiment_runs (
                run_id, pair_id, run_kind, variant,
                sender_target_pps, udp_payload_bytes,
                window_secs, sample_interval_ms,
                started_at_utc, stopped_at_utc, status, status_reason, source_note
            ) VALUES (
                $1, $2, $3, $4,
                $5, 8,
                $6, $7,
                now(), now(), 'complete', $8, $9
            )
            "#,
            &[
                &run_id,
                &pair_id,
                &run_kind,
                &path.as_str(),
                &(host_pps.round() as i64),
                &(window.as_secs_f64()),
                &(sample.as_millis() as i32),
                &format!("rxcnt_delta={rxcnt_delta} host_pps={host_pps:.0}"),
                &SOURCE_NOTE,
            ],
        )
        .await
        .context("INSERT experiment_runs")?;

    for s in samples {
        client
            .execute(
                r#"
                INSERT INTO counter_samples (run_id, time_ns, counter_value)
                VALUES ($1, $2, $3)
                ON CONFLICT (run_id, time_ns) DO UPDATE
                  SET counter_value = EXCLUDED.counter_value
                "#,
                &[&run_id, &s.t_ns, &(s.rxcnt as i64)],
            )
            .await
            .context("INSERT counter_samples")?;
    }
    Ok(())
}

async fn settle_after_reboot() {
    eprintln!(
        "    settle {}s after flash / reset …",
        REBOOT_SETTLE.as_secs()
    );
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

async fn read_snapshot(agent: &AgentClient) -> Result<Snapshot> {
    let rxcnt = parse_rxcnt(&agent.request(&format!("rxcnt {IPPROTO_UDP}")).await?)?;
    let net_rx = parse_net_rx(&agent.counters().await?)?;
    Ok(Snapshot { rxcnt, net_rx })
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

fn parse_net_rx(line: &str) -> Result<u64> {
    let line = line.trim();
    let body = line
        .strip_prefix("OK counters ")
        .or_else(|| line.strip_prefix("OK COUNTERS "))
        .unwrap_or(line);
    for part in body.split_whitespace() {
        if let Some(v) = part.strip_prefix("net_rx=") {
            return v.parse().context("net_rx=");
        }
    }
    bail!("missing net_rx=");
}
