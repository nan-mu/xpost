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
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::bench::udp_blast;
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
const IPPROTO_UDP: u32 = 17;
const SOURCE_NOTE: &str = "xpost-test just-tx-pps";
const FLASH_IDLE: Duration = Duration::from_secs(300);
const PERF_WINDOW: Duration = Duration::from_secs(40);
const PERF_SMOKE_WINDOW: Duration = Duration::from_secs(10);
const DRAIN_MS: u64 = 200;
const RX_READY_TRIES: usize = 40;
const RX_READY_BURST: u32 = 64;

pub struct JustTxPpsOpts {
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
        .context("just-tx-pps needs [agent].endpoint in config/default.toml")?;
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

    RoundUi::banner(&format!(
        "just-tx-pps arms={:?} agent={endpoint} esp={esp_s} window={}s sample={}ms pairs={rounds} formal={formal} pg={}",
        paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        window.as_secs(),
        sample.as_millis(),
        !opts.no_pg
    ));

    let mut pg = if opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts { formal };
        Some(pg_dsn::connect(pg_opts).await?)
    };

    let n_rounds = rounds * paths.len();
    let mut eta = BudgetEta::new();
    eta.add_flashes(n_rounds);
    let per_arm = READY_BUDGET + window + Duration::from_millis(DRAIN_MS);
    eta.add_fixed(per_arm.saturating_mul(n_rounds as u32));

    let ui = RoundUi::new();
    let mut first_round = true;

    for pair_id in 1..=rounds {
        for path in &paths {
            run_arm(
                cfg,
                &agent,
                &ui,
                &mut eta,
                *path,
                pair_id as i32,
                window,
                sample,
                run_kind,
                formal,
                first_round,
                pg.as_mut(),
            )
            .await
            .with_context(|| format!("pair={pair_id} path={}", path.as_str()))?;
            first_round = false;
        }
    }

    RoundUi::banner(&format!(
        "just-tx-pps done pairs={rounds} arms={}",
        paths.len()
    ));
    Ok(())
}

async fn run_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    pair_id: i32,
    window: Duration,
    sample: Duration,
    run_kind: &str,
    formal: bool,
    first_round: bool,
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

    ui.begin_round();
    ui.set_eta(&eta.fmt());

    if first_round {
        if let Some(client) = pg.as_ref() {
            ui.check_start("清除旧行 / 确保 experiment_*");
            let pg_opts = PgOpts { formal };
            let n = pg_dsn::prepare_experiment_pps_note(client, pg_opts, SOURCE_NOTE)
                .await
                .context("prepare experiment pps")?;
            ui.check_ok(&format!("删 {n} runs"));
        }
    }

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

    if let Err(e) = wait_rx_ready(cfg, agent, ui, path).await {
        ui.clear_temp_for_error();
        return Err(e.context(format!("RX ready after flash `{profile}`")));
    }
    ui.check_ok("rx ready");

    let run_id = Uuid::new_v4();
    let measure = measure_round(cfg, agent, ui, eta, path, pair_id, window, sample).await;
    let (samples, rxcnt_delta, net_rx_delta, host_pps, elapsed) = match measure {
        Ok(v) => v,
        Err(e) => {
            ui.clear_temp_for_error();
            return Err(e);
        }
    };

    ui.permanent(&format!(
        "    {} pair={pair_id}: rxcntΔ={rxcnt_delta} net_rxΔ={net_rx_delta} host_pps={host_pps:.0} elapsed={elapsed:.2}s samples={}",
        path.as_str(),
        samples.len()
    ));

    eta.consume_fixed(window + Duration::from_millis(DRAIN_MS));
    ui.set_eta(&eta.fmt());

    if let Some(client) = pg {
        ui.check_start("写入 postgres");
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
        ui.check_ok(&format!("1 run, {} samples", samples.len()));
    }

    ui.end_round();
    Ok(())
}

async fn measure_round(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    pair_id: i32,
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

    ui.check_start(&format!("blast pair={pair_id}"));
    ui.live_set(
        &format!("pair {pair_id} arm={}", path.as_str()),
        &[("tx", 0.0, &format!("0/{:.0}s", window.as_secs_f64()))],
    );
    ui.set_eta(&eta.fmt());

    let deadline = t0 + window + Duration::from_millis(DRAIN_MS);
    while Instant::now() < deadline {
        tokio::time::sleep(sample).await;
        let s = read_snapshot(agent).await.context("snapshot sample")?;
        samples.push(Sample {
            t_ns: t0.elapsed().as_nanos() as i64,
            rxcnt: s.rxcnt,
        });
        let elapsed = t0.elapsed().as_secs_f64().min(window.as_secs_f64());
        let tx_frac = (elapsed / window.as_secs_f64().max(1e-6)).clamp(0.0, 1.0);
        let rx_delta = s.rxcnt.saturating_sub(first.rxcnt);
        ui.live_set(
            &format!("pair {pair_id} arm={}", path.as_str()),
            &[(
                "tx",
                tx_frac,
                &format!("{:.0}/{:.0}s", elapsed, window.as_secs_f64()),
            )],
        );
        ui.check_note(&format!("rxcntΔ={rx_delta}"));
        ui.set_eta(&eta.fmt());
        if blast_handle.is_finished() {
            break;
        }
    }

    let blast = blast_handle
        .await
        .context("blast join")?
        .context("udp_blast")?;
    ui.live_clear();
    ui.check_ok(&format!(
        "{:.1}s host_pps={:.0}",
        blast.elapsed_secs(),
        blast.host_pps()
    ));

    ui.check_start("等待 DUT rxcnt");
    tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
    let mut last = read_snapshot(agent).await.context("snapshot end")?;
    let wait_deadline = Instant::now() + Duration::from_secs(3);
    while last.rxcnt <= first.rxcnt && Instant::now() < wait_deadline {
        ui.check_note(&format!(
            "rxcnt={} (need >{})",
            last.rxcnt, first.rxcnt
        ));
        tokio::time::sleep(Duration::from_millis(250)).await;
        last = read_snapshot(agent).await.context("snapshot wait")?;
    }
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
        ui.check_fail("rxcnt decreased");
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
        if rxcnt_delta == 0 || net_rx_delta * 20 > rxcnt_delta {
            ui.check_fail(&format!("net_rxΔ={net_rx_delta}"));
            bail!(
                "xdp path: net_rx moved under just_tx (net_rxΔ={net_rx_delta} rxcntΔ={rxcnt_delta}); stack saw traffic"
            );
        }
        ui.permanent(&format!(
            "    warn: xdp net_rxΔ={net_rx_delta} (rxcntΔ={rxcnt_delta}); continuing"
        ));
    }
    if rxcnt_delta == 0 {
        ui.check_fail(&format!(
            "silent (rxcnt {}→{}, net_rxΔ={net_rx_delta}, host_pps={host_pps:.0})",
            first.rxcnt, last.rxcnt
        ));
        bail!(
            "rxcnt 17 did not increase during blast \
             (rxcnt {}→{}, net_rxΔ={net_rx_delta}, host_pps={host_pps:.0}, path={})",
            first.rxcnt,
            last.rxcnt,
            path.as_str()
        );
    }
    ui.check_ok(&format!("rxcntΔ={rxcnt_delta} net_rxΔ={net_rx_delta}"));

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

async fn wait_rx_ready(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    path: PpsPath,
) -> Result<()> {
    let esp = cfg.target.esp_ip;
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => iface_mac(&cfg.probe.iface)?,
    };
    let raw = Arc::new(
        RawTx::open(
            &cfg.probe.iface,
            src_mac,
            esp_mac,
            esp,
            STIM_PORT,
            cfg.target.stim_sport,
        )
        .context("RawTx::open for RX ready")?,
    );
    let spoof = gen_lan_ip(esp, &[esp]);

    for i in 0..RX_READY_TRIES {
        let before = read_snapshot(agent).await.context("RX ready snapshot")?;
        let raw_c = Arc::clone(&raw);
        let n = RX_READY_BURST;
        tokio::task::spawn_blocking(move || {
            for _ in 0..n {
                let _ = raw_c.send_udp(spoof, esp, STIM_PORT, b"READY");
            }
        })
        .await
        .context("RX ready burst join")?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let after = read_snapshot(agent).await.context("RX ready after")?;
        if after.rxcnt > before.rxcnt {
            ui.check_note(&format!(
                "rxcnt {}→{} net_rx {}→{}",
                before.rxcnt, after.rxcnt, before.net_rx, after.net_rx
            ));
            tokio::time::sleep(Duration::from_secs(2)).await;
            return Ok(());
        }
        if i % 5 == 4 {
            ui.check_note(&format!(
                "try{i} quiet (rxcnt={} net_rx={})",
                after.rxcnt, after.net_rx
            ));
        }
    }
    bail!(
        "DUT never counted RX after flash (path={}); check Wi-Fi / esp_ip / esp_mac",
        path.as_str()
    );
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
