//! `probe just-drop` → functional ACK path (`xdp_ack_*`) then optional UDP blast perf.
//!
//! Functional: N stims + quiet baseline so background ARP/DHCP cannot fake a pass.
//! Perf: TR3000 `udp_blast` at link rate; DUT `drop` delta / window → dut_pps.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::info;
use rand::RngExt;

use crate::agent::AgentClient;
use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::udp_blast::{self, BLAST_THREADS};
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
/// Functional stims per phase — large vs ambient Wi-Fi noise.
const STIM_N: u32 = 200;
/// Quiet window to estimate ambient `drop` rate before asserting stim signal.
const QUIET: Duration = Duration::from_secs(2);
/// Drain after fire-and-forget burst before reading counters / map dump.
const BURST_DRAIN: Duration = Duration::from_millis(500);
/// Allow a few ambient frames in counter deltas.
const SLACK: u64 = 5;
/// UDP proto key in just_drop `rxcnt` ARRAY.
const RXCNT_UDP: u32 = 17;

const DEFAULT_ELF: &str = "/Users/nan/bs/xpost/examples/just_xdp/just_drop.elf";
const REBOOT_SETTLE: Duration = Duration::from_secs(20);
/// Match drop-xdp formal window: long enough to approach wireless ceiling.
const PERF_WINDOW: Duration = Duration::from_secs(40);
const PERF_SMOKE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug)]
struct Counters {
    drop: u64,
    net_rx: u64,
}

pub struct JustDropOpts {
    pub formal: bool,
    pub no_pg: bool,
    pub no_perf: bool,
    pub smoke: bool,
    pub window_secs: Option<u64>,
}

pub async fn run_just_drop(cfg: &FileConfig, opts: JustDropOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("just-drop needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp = cfg.target.esp_ip.to_string();
    let elf = std::env::var("XPOST_JUST_DROP_ELF").unwrap_or_else(|_| DEFAULT_ELF.into());
    let stim_n = if opts.smoke { 10 } else { STIM_N };

    eprintln!(
        "just-drop agent={endpoint} esp={esp} stim_port={STIM_PORT} stim_n={stim_n} formal={} perf={}",
        opts.formal,
        !opts.no_perf
    );

    let mut pg = if opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts {
            formal: opts.formal,
        };
        let client = pg_dsn::connect(pg_opts).await?;
        pg_dsn::prepare(&client, pg_opts, TableSet::Ack).await?;
        Some(client)
    };

    // —— Phase A: clear → N acks, net_rx ≈ N ——
    info!("phase none: clear cold table");
    agent.clear_xdp().await.context("clear")?;
    settle_after_reboot().await;
    let (pass_none, before_n, after_n, acks) =
        phase_none(&agent, &esp, stim_n).await.context("phase none")?;
    eprintln!(
        "  loaded=none acks={acks}/{stim_n} drop {}→{} net_rx {}→{} pass={pass_none}",
        before_n.drop, after_n.drop, before_n.net_rx, after_n.net_rx
    );
    if let Some(client) = pg.as_mut() {
        ingest(
            client, "none", STIM_PORT, stim_n, true, acks, before_n, after_n, pass_none,
        )
        .await?;
    }
    if !pass_none {
        bail!("just-drop phase none failed (want {stim_n} ack + net_rx≈{stim_n})");
    }

    // —— Phase B: drop XDP → burst N, no-ack sample, drop/rxcnt signal ——
    info!("phase drop: program {elf}");
    agent.program(&elf).await.context("program")?;
    settle_after_reboot().await;
    let (pass_drop, before_d, after_d, sent, rx_delta) =
        phase_drop(&agent, &esp, stim_n).await.context("phase drop")?;
    eprintln!(
        "  loaded=drop sent={sent} rxcnt[{RXCNT_UDP}]Δ={rx_delta} drop {}→{} net_rx {}→{} pass={pass_drop}",
        before_d.drop, after_d.drop, before_d.net_rx, after_d.net_rx
    );
    if let Some(client) = pg.as_mut() {
        ingest(
            client,
            "drop",
            STIM_PORT,
            stim_n,
            false,
            0,
            before_d,
            after_d,
            pass_drop,
        )
        .await?;
    }
    if !pass_drop {
        bail!(
            "just-drop phase drop failed (want burst + drop/rxcnt signal ≥{stim_n}, net_rx flat)"
        );
    }

    eprintln!("just-drop functional ok");

    if opts.no_perf {
        return Ok(());
    }

    // —— Perf: link-rate UDP blast while drop XDP is still loaded ——
    let window = opts
        .window_secs
        .map(Duration::from_secs)
        .unwrap_or(if opts.smoke {
            PERF_SMOKE_WINDOW
        } else {
            PERF_WINDOW
        });
    run_perf(cfg, &agent, window).await.context("just-drop perf")?;
    eprintln!("just-drop ok (functional + perf)");
    Ok(())
}

async fn settle_after_reboot() {
    eprintln!("  settle {}s after DUT reset …", REBOOT_SETTLE.as_secs());
    tokio::time::sleep(REBOOT_SETTLE).await;
}

async fn phase_none(
    agent: &AgentClient,
    esp: &str,
    stim_n: u32,
) -> Result<(bool, Counters, Counters, u32)> {
    let _ambient = quiet_drop_delta(agent).await?;
    let before = parse_counters(&agent.counters().await.context("counters before none")?)?;
    let mut acks = 0u32;
    for i in 0..stim_n {
        let got = agent.probe(esp, STIM_PORT).await.with_context(|| format!("probe none #{i}"))?;
        if got == "ack" {
            acks += 1;
        }
    }
    let after = parse_counters(&agent.counters().await.context("counters after none")?)?;
    let net_d = after.net_rx.saturating_sub(before.net_rx);
    let pass = acks == stim_n
        && net_d >= u64::from(stim_n)
        && net_d <= u64::from(stim_n) + SLACK
        && after.drop == before.drop;
    Ok((pass, before, after, acks))
}

async fn phase_drop(
    agent: &AgentClient,
    esp: &str,
    stim_n: u32,
) -> Result<(bool, Counters, Counters, u32, u64)> {
    let ambient = quiet_drop_delta(agent).await?;
    let before = parse_counters(&agent.counters().await.context("counters before drop")?)?;
    let rx_before = agent.map_slot("rxcnt", RXCNT_UDP).await?;

    let t0 = Instant::now();
    let sent = agent
        .probe_burst(esp, STIM_PORT, stim_n)
        .await
        .context("probe_burst")?;
    tokio::time::sleep(BURST_DRAIN).await;
    let elapsed = t0.elapsed().as_secs_f64().max(1e-3);

    // Spot-check: a few short probes must be no-ack (not waiting 2s × N).
    let mut noacks = 0u32;
    for i in 0..3u32 {
        let got = agent
            .probe(esp, STIM_PORT)
            .await
            .with_context(|| format!("probe drop sample #{i}"))?;
        if got == "no-ack" {
            noacks += 1;
        }
    }

    let after = parse_counters(&agent.counters().await.context("counters after drop")?)?;
    let rx_after = agent.map_slot("rxcnt", RXCNT_UDP).await?;
    let drop_d = after.drop.saturating_sub(before.drop);
    let rx_d = rx_after.saturating_sub(rx_before);
    // Ambient extrapolation over the burst window (plus sample probes).
    let expected_bg = (ambient as f64 * (elapsed + 6.0) / QUIET.as_secs_f64()).ceil() as u64;
    let drop_signal = drop_d.saturating_sub(expected_bg);

    let pass = sent == stim_n
        && noacks == 3
        && after.net_rx == before.net_rx
        && drop_signal >= u64::from(stim_n)
        && rx_d >= u64::from(stim_n);
    if !pass {
        eprintln!(
            "  drop detail: ambient_quietΔ={ambient} expected_bg≈{expected_bg} dropΔ={drop_d} signal={drop_signal} rxΔ={rx_d} noacks={noacks}/3"
        );
    }
    Ok((pass, before, after, sent, rx_d))
}

async fn quiet_drop_delta(agent: &AgentClient) -> Result<u64> {
    let a = parse_counters(&agent.counters().await.context("quiet counters a")?)?;
    tokio::time::sleep(QUIET).await;
    let b = parse_counters(&agent.counters().await.context("quiet counters b")?)?;
    Ok(b.drop.saturating_sub(a.drop))
}

async fn run_perf(cfg: &FileConfig, agent: &AgentClient, window: Duration) -> Result<()> {
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
    .context("RawTx::open for just-drop perf")?;
    let spoof = gen_lan_ip(esp, &[esp]);

    let before = parse_counters(&agent.counters().await.context("perf counters before")?)?;
    eprintln!(
        "  perf blast {}s  UDP×{BLAST_THREADS} 64B  iface={}  spoof={spoof} → host/dut pps",
        window.as_secs(),
        cfg.probe.iface
    );
    let blast = tokio::task::spawn_blocking({
        let raw = Arc::new(raw);
        move || udp_blast::run_blast_window(raw, spoof, esp, STIM_PORT, window)
    })
    .await
    .context("blast join")?
    .context("udp_blast")?;
    // Brief drain for in-flight frames.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = parse_counters(&agent.counters().await.context("perf counters after")?)?;

    let drop_d = after.drop.saturating_sub(before.drop);
    let host_pps = blast.host_pps();
    let dut_pps = drop_d as f64 / blast.elapsed_secs();
    eprintln!(
        "  perf host_pps={host_pps:.0}  dut_pps={dut_pps:.0}  tx={}  dropΔ={drop_d}  net_rx {}→{}",
        blast.tx_count, before.net_rx, after.net_rx
    );
    if host_pps < 5_000.0 {
        eprintln!(
            "  WARN: host blast only {host_pps:.0} pps — probe may be CPU-limited (want wireless ceiling)"
        );
    }
    if after.net_rx != before.net_rx {
        bail!("perf: net_rx moved under drop XDP (stack saw traffic)");
    }
    if drop_d == 0 {
        bail!("perf: drop did not increase during blast");
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

#[allow(clippy::too_many_arguments)]
async fn ingest(
    client: &tokio_postgres::Client,
    loaded: &str,
    stim_port: u16,
    stim_n: u32,
    expect_ack: bool,
    ack_count: u32,
    before: Counters,
    after: Counters,
    pass: bool,
) -> Result<()> {
    let row = client
        .query_one(
            "INSERT INTO xdp_ack_config (loaded, stim_port, stim_n) VALUES ($1, $2, $3) RETURNING id",
            &[&loaded, &(stim_port as i32), &(stim_n as i32)],
        )
        .await
        .context("insert xdp_ack_config")?;
    let config_id: i64 = pg_dsn::config_id_from_row(&row)?;
    client
        .execute(
            "INSERT INTO xdp_ack_trial \
             (config_id, expect_ack, got_ack, ack_count, drop_before, drop_after, net_rx_before, net_rx_after, pass) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            &[
                &config_id,
                &expect_ack,
                &(ack_count > 0),
                &(ack_count as i32),
                &(before.drop as i64),
                &(after.drop as i64),
                &(before.net_rx as i64),
                &(after.net_rx as i64),
                &pass,
            ],
        )
        .await
        .context("insert xdp_ack_trial")?;
    Ok(())
}
