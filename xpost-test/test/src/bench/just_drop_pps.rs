//! `just_drop` — Counter full-rate + Aggregate/Gauge/Trace fixed-pps rounds.
//!
//! Arms: `hal` (`just-drop-hal-*` + `drop on`) and `xdp` (`just-drop-*` + xdp.elf).
//! Kinds: `c` → `just_drop_config`/`data`; `a`/`g`/`t` → `just_drop_run` + children.
//! Formal: DROP same-named tables then CREATE empty (`pg_dsn::prepare` + `TableSet::JustDrop`).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;
use serde_json::{Number, Value as JsonValue, json};
use tokio_postgres::Client;

use crate::agent::AgentClient;
use crate::bench::counters;
use crate::bench::pg_dsn::{self, PgOpts, TableSet};
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::bench::udp_blast;
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const PERF_WINDOW: Duration = Duration::from_secs(40);
const PERF_SMOKE_WINDOW: Duration = Duration::from_secs(10);
const DRAIN_MS: u64 = 200;
const DEFAULT_FIXED_PPS: u64 = 800;
const DEFAULT_TRACE_PPS: u64 = 200;
const DEFAULT_INSTR_ROUNDS: usize = 10;
const DEFAULT_INSTR_WINDOW_SECS: u64 = 12;

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
            other => bail!("path must be xdp|hal, got {other}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xdp => "xdp",
            Self::Hal => "hal",
        }
    }

    pub fn drop_site(self) -> &'static str {
        match self {
            Self::Xdp => "xdp",
            Self::Hal => "app",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropKind {
    Counter,
    Aggregate,
    Gauge,
    Trace,
}

impl DropKind {
    fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "c" | "counter" => Ok(Self::Counter),
            "a" | "aggregate" => Ok(Self::Aggregate),
            "g" | "gauge" => Ok(Self::Gauge),
            "t" | "trace" => Ok(Self::Trace),
            other => bail!("kind must be c|a|g|t, got {other}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "c",
            Self::Aggregate => "a",
            Self::Gauge => "g",
            Self::Trace => "t",
        }
    }

    fn profile(self, path: PpsPath) -> &'static str {
        match (path, self) {
            (PpsPath::Xdp, Self::Counter) => "just-drop-c",
            (PpsPath::Xdp, Self::Aggregate) => "just-drop-a",
            (PpsPath::Xdp, Self::Gauge) => "just-drop-g",
            (PpsPath::Xdp, Self::Trace) => "just-drop-t",
            (PpsPath::Hal, Self::Counter) => "just-drop-hal-c",
            (PpsPath::Hal, Self::Aggregate) => "just-drop-hal-a",
            (PpsPath::Hal, Self::Gauge) => "just-drop-hal-g",
            (PpsPath::Hal, Self::Trace) => "just-drop-hal-t",
        }
    }
}

pub struct JustDropPpsOpts {
    /// Arms to run (order preserved). Empty → both from embedded config.
    pub paths: Vec<PpsPath>,
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub sample_interval_ms: u64,
    pub rounds: Option<usize>,
}

#[derive(Clone, Debug)]
struct Sample {
    t: SystemTime,
    drop: u64,
}

pub async fn run_just_drop_pps(cfg: &FileConfig, opts: JustDropPpsOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("just_drop needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();

    let paths = if opts.paths.is_empty() {
        cfg.just_drop
            .arms
            .iter()
            .map(|s| PpsPath::parse(s))
            .collect::<Result<Vec<_>>>()?
    } else {
        opts.paths
    };
    if paths.is_empty() {
        bail!("just_drop: no arms (set just_drop.toml arms or --path)");
    }

    let kinds = cfg
        .just_drop
        .kinds
        .iter()
        .map(|s| DropKind::parse(s))
        .collect::<Result<Vec<_>>>()?;
    if kinds.is_empty() {
        bail!("just_drop: kinds empty (set just_drop.toml kinds)");
    }

    let formal = opts.formal || cfg.just_drop.formal;
    let sample = Duration::from_millis(opts.sample_interval_ms.max(100));

    let c_window = opts
        .window_secs
        .map(Duration::from_secs)
        .or_else(|| cfg.just_drop.window_secs.map(Duration::from_secs))
        .unwrap_or(if opts.smoke {
            PERF_SMOKE_WINDOW
        } else {
            PERF_WINDOW
        });
    let c_rounds = opts.rounds.unwrap_or(if opts.smoke {
        1
    } else {
        cfg.just_drop.rounds.max(1)
    });

    let instr_window = Duration::from_secs(
        cfg.just_drop
            .instr_window_secs
            .unwrap_or(if opts.smoke { 5 } else { DEFAULT_INSTR_WINDOW_SECS }),
    );
    let instr_rounds = if opts.smoke {
        1
    } else {
        cfg.just_drop
            .instr_rounds
            .unwrap_or(DEFAULT_INSTR_ROUNDS)
            .max(1)
    };
    let fixed_pps = cfg.just_drop.fixed_pps.unwrap_or(DEFAULT_FIXED_PPS) as f64;
    let trace_pps = cfg.just_drop.trace_pps.unwrap_or(DEFAULT_TRACE_PPS) as f64;

    RoundUi::banner(&format!(
        "just_drop arms={:?} kinds={:?} agent={endpoint} esp={esp_s} formal={formal} pg={}",
        paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        kinds.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
        !opts.no_pg
    ));

    let mut pg = if opts.no_pg {
        None
    } else {
        let pg_opts = PgOpts { formal };
        Some(pg_dsn::connect(pg_opts).await?)
    };

    // One flash per (kind × arm); rounds reuse the image (perf clear between).
    let n_flashes = kinds.len() * paths.len();
    let mut eta = BudgetEta::new();
    eta.add_flashes(n_flashes);
    for kind in &kinds {
        let (window, r) = match kind {
            DropKind::Counter => (c_window, c_rounds),
            _ => (instr_window, instr_rounds),
        };
        // settle once per flash; window × rounds after that
        let settle = READY_BUDGET.saturating_mul(paths.len() as u32);
        let measure = (window + Duration::from_millis(DRAIN_MS))
            .saturating_mul((r * paths.len()) as u32);
        eta.add_fixed(settle.saturating_add(measure));
    }

    let ui = RoundUi::new();
    let mut first_flash = true;

    for kind in &kinds {
        let (rounds, window, target_pps) = match kind {
            DropKind::Counter => (c_rounds, c_window, None),
            DropKind::Trace => (instr_rounds, instr_window, Some(trace_pps)),
            _ => (instr_rounds, instr_window, Some(fixed_pps)),
        };
        for path in &paths {
            flash_and_arm(
                cfg,
                &agent,
                &ui,
                &mut eta,
                *path,
                *kind,
                formal,
                first_flash,
                pg.as_mut(),
            )
            .await
            .with_context(|| {
                format!("flash arm={} kind={}", path.as_str(), kind.as_str())
            })?;
            first_flash = false;

            for round in 1..=rounds {
                ui.begin_round();
                ui.set_eta(&eta.fmt());
                let result = match kind {
                    DropKind::Counter => {
                        run_counter_arm(cfg, &agent, &ui, &eta, *path, window, sample, pg.as_mut())
                            .await
                    }
                    DropKind::Aggregate | DropKind::Gauge | DropKind::Trace => {
                        let pps = target_pps.context("instr kind needs target_pps")?;
                        run_instr_arm(cfg, &agent, &ui, &eta, *path, *kind, window, pps, pg.as_mut())
                            .await
                    }
                };
                if let Err(e) = result {
                    ui.clear_temp_for_error();
                    return Err(e).with_context(|| {
                        format!(
                            "arm={} kind={} round={round}",
                            path.as_str(),
                            kind.as_str()
                        )
                    });
                }
                eta.consume_fixed(window + Duration::from_millis(DRAIN_MS));
                ui.set_eta(&eta.fmt());
                ui.end_round();
            }
        }
    }

    RoundUi::banner(&format!(
        "just_drop done kinds={} arms={} flashes={}",
        kinds.len(),
        paths.len(),
        n_flashes
    ));
    Ok(())
}

/// Flash once + settle + HAL `drop on`. Measure rounds happen outside.
async fn flash_and_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    kind: DropKind,
    formal: bool,
    first_flash: bool,
    pg: Option<&mut Client>,
) -> Result<()> {
    let profile = kind.profile(path);

    ui.begin_round();
    ui.set_eta(&eta.fmt());

    if first_flash {
        if let Some(client) = pg.as_ref() {
            ui.check_start("DROP+CREATE just_drop_*");
            let pg_opts = PgOpts { formal };
            pg_dsn::prepare(client, pg_opts, TableSet::JustDrop)
                .await
                .context("prepare just_drop")?;
            ui.check_ok(if formal { "recreated" } else { "ok" });
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

    if path == PpsPath::Hal {
        ui.check_start("drop on");
        let msg = agent.request("drop on").await.context("drop on")?;
        if !msg.contains("OK drop on") {
            ui.check_fail(&msg);
            ui.clear_temp_for_error();
            bail!("expected OK drop on after just-drop-hal flash, got: {msg}");
        }
        ui.check_ok("");
    }

    ui.end_round();
    Ok(())
}

async fn run_counter_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    sample: Duration,
    pg: Option<&mut Client>,
) -> Result<()> {
    let counters_before = counters::fetch_counters(agent).await?;
    let started_at = SystemTime::now();

    let (samples, drop_delta, net_rx_delta, host_pps, sent) =
        measure_counter_round(cfg, agent, ui, eta, path, window, sample).await?;
    let stopped_at = SystemTime::now();
    let counters_after = counters::fetch_counters(agent).await?;
    let counters_json = counters::counters_delta(&counters_before, &counters_after);

    ui.permanent(&format!(
        "    {}/c: dropΔ={drop_delta} net_rxΔ={net_rx_delta} host_pps={host_pps:.0} sent={sent} samples={}",
        path.as_str(),
        samples.len()
    ));

    if let Some(client) = pg {
        ui.check_start("写入 postgres");
        ingest_counter(
            client,
            path,
            host_pps,
            started_at,
            stopped_at,
            sent,
            &counters_json,
            &samples,
        )
        .await?;
        ui.check_ok(&format!("1 config, {} samples", samples.len()));
    }
    Ok(())
}

async fn run_instr_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    kind: DropKind,
    window: Duration,
    target_pps: f64,
    pg: Option<&mut Client>,
) -> Result<()> {
    let _ = agent.request("perf clear").await;
    let counters_before = counters::fetch_counters(agent).await?;
    let started_at = SystemTime::now();

    let sent = paced_blast(cfg, ui, eta, path, window, target_pps).await?;
    tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
    let stopped_at = SystemTime::now();
    let counters_after = counters::fetch_counters(agent).await?;
    let counters_json = counters::counters_delta(&counters_before, &counters_after);

    let host_pps = sent as f64 / window.as_secs_f64().max(1e-6);
    ui.permanent(&format!(
        "    {}/{}: target_pps={target_pps:.0} host_pps={host_pps:.0} sent={sent}",
        path.as_str(),
        kind.as_str()
    ));

    if let Some(client) = pg {
        ui.check_start("写入 postgres");
        let run_id = ingest_run_meta(
            client,
            path,
            kind,
            target_pps,
            window.as_secs_f64(),
            started_at,
            stopped_at,
            sent,
            &counters_json,
        )
        .await?;
        match kind {
            DropKind::Aggregate => {
                let line = agent.request("agg").await.context("agg")?;
                ingest_agg(client, run_id, &line).await?;
                ui.check_ok("run+agg");
            }
            DropKind::Gauge => {
                let line = agent.request("gauge").await.context("gauge")?;
                ingest_gauge(client, run_id, &line).await?;
                ui.check_ok("run+gauge");
            }
            DropKind::Trace => {
                let line = agent.request("trace").await.context("trace")?;
                ingest_trace(client, run_id, &line).await?;
                ui.check_ok("run+trace");
            }
            DropKind::Counter => unreachable!(),
        }
    } else {
        // Still exercise serial cmds without PG.
        match kind {
            DropKind::Aggregate => {
                let _ = agent.request("agg").await?;
            }
            DropKind::Gauge => {
                let _ = agent.request("gauge").await?;
            }
            DropKind::Trace => {
                let _ = agent.request("trace").await?;
            }
            DropKind::Counter => {}
        }
    }

    let _ = agent.request("perf clear").await;
    Ok(())
}

async fn measure_counter_round(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    sample: Duration,
) -> Result<(Vec<Sample>, u64, u64, f64, u64)> {
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

    let wall0 = SystemTime::now();
    let t0 = Instant::now();
    let first_line = agent.counters().await.context("counters t0")?;
    let first = parse_drop_net_rx(&first_line)?;
    let mut samples = vec![Sample {
        t: wall0,
        drop: first.0,
    }];

    let blast_handle = tokio::task::spawn_blocking({
        let raw = Arc::new(raw);
        move || udp_blast::run_blast_window(raw, spoof, esp, STIM_PORT, window)
    });

    ui.live_set(
        &format!("arm={}", path.as_str()),
        &[
            ("tx", 0.0, &format!("0/{:.0}s", window.as_secs_f64())),
            ("drop", 0.0, "dropΔ=0 net_rxΔ=0"),
        ],
    );
    ui.set_eta(&eta.fmt());

    let deadline = t0 + window + Duration::from_millis(DRAIN_MS);
    while Instant::now() < deadline {
        tokio::time::sleep(sample).await;
        let line = agent.counters().await.context("counters sample")?;
        let (drop, net_rx) = parse_drop_net_rx(&line)?;
        let now = SystemTime::now();
        samples.push(Sample { t: now, drop });
        let elapsed = t0.elapsed().as_secs_f64().min(window.as_secs_f64());
        let tx_frac = (elapsed / window.as_secs_f64().max(1e-6)).clamp(0.0, 1.0);
        let drop_delta = drop.saturating_sub(first.0);
        let net_rx_delta = net_rx.saturating_sub(first.1);
        let drop_frac = if net_rx_delta > 0 {
            (drop_delta as f64 / net_rx_delta as f64).clamp(0.0, 1.0)
        } else if drop_delta > 0 {
            tx_frac
        } else {
            0.0
        };
        ui.live_set(
            &format!("arm={}", path.as_str()),
            &[
                (
                    "tx",
                    tx_frac,
                    &format!("{:.0}/{:.0}s", elapsed, window.as_secs_f64()),
                ),
                (
                    "drop",
                    drop_frac,
                    &format!("dropΔ={drop_delta} net_rxΔ={net_rx_delta}"),
                ),
            ],
        );
        ui.set_eta(&eta.fmt());
        if blast_handle.is_finished() {
            break;
        }
    }

    let blast = blast_handle
        .await
        .context("blast join")?
        .context("udp_blast")?;
    tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
    let last_line = agent.counters().await.context("counters end")?;
    let last = parse_drop_net_rx(&last_line)?;
    let t_end = SystemTime::now();
    if samples
        .last()
        .map(|s| s.drop != last.0)
        .unwrap_or(true)
    {
        samples.push(Sample {
            t: t_end,
            drop: last.0,
        });
    }

    ui.live_clear();

    if last.0 < first.0 {
        bail!(
            "counter decreased {} → {} (reboot/wrap?)",
            first.0,
            last.0
        );
    }
    let drop_delta = last.0 - first.0;
    let net_rx_delta = last.1.saturating_sub(first.1);
    let host_pps = blast.host_pps();
    let sent = blast.tx_count;

    if path == PpsPath::Xdp && net_rx_delta != 0 {
        bail!("xdp path: net_rx moved under drop ({net_rx_delta}); stack saw traffic");
    }
    if drop_delta == 0 {
        bail!("drop did not increase during blast");
    }

    Ok((samples, drop_delta, net_rx_delta, host_pps, sent))
}

async fn paced_blast(
    cfg: &FileConfig,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    target_pps: f64,
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
    let spoof = gen_lan_ip(esp, &[esp]);

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
        if raw.send_udp(spoof, esp, STIM_PORT, b"D").is_ok() {
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

async fn ingest_counter(
    client: &mut Client,
    path: PpsPath,
    target_pps: f64,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: u64,
    counters: &JsonValue,
    samples: &[Sample],
) -> Result<()> {
    let row = client
        .query_one(
            r#"
            INSERT INTO just_drop_config (
                fw_type, target_pps, started_at, stopped_at, sent, counters
            ) VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
            &[
                &path.as_str(),
                &target_pps,
                &started_at,
                &stopped_at,
                &(sent as i64),
                counters,
            ],
        )
        .await
        .context("INSERT just_drop_config")?;
    let config_id = pg_dsn::config_id_from_row(&row)?;

    for s in samples {
        client
            .execute(
                r#"
                INSERT INTO just_drop_data (config_id, t, counter_value)
                VALUES ($1, $2, $3)
                "#,
                &[&config_id, &s.t, &(s.drop as i64)],
            )
            .await
            .context("INSERT just_drop_data")?;
    }
    Ok(())
}

async fn ingest_run_meta(
    client: &mut Client,
    path: PpsPath,
    kind: DropKind,
    target_pps: f64,
    window_s: f64,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: u64,
    counters: &JsonValue,
) -> Result<i64> {
    let row = client
        .query_one(
            r#"
            INSERT INTO just_drop_run (
                fw_type, drop_site, kind, target_pps, window_s,
                started_at, stopped_at, sent, counters
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id
            "#,
            &[
                &path.as_str(),
                &path.drop_site(),
                &kind.as_str(),
                &target_pps,
                &window_s,
                &started_at,
                &stopped_at,
                &(sent as i64),
                counters,
            ],
        )
        .await
        .context("INSERT just_drop_run")?;
    pg_dsn::config_id_from_row(&row)
}

async fn ingest_agg(client: &mut Client, run_id: i64, line: &str) -> Result<()> {
    let body = line
        .trim()
        .strip_prefix("OK agg")
        .context("expected OK agg")?;
    for seg in body.split('|').map(str::trim).filter(|s| !s.is_empty()) {
        let mut name: Option<String> = None;
        let mut count = 0i64;
        let mut total = 0i64;
        let mut min = 0i64;
        let mut max = 0i64;
        let mut lo = 0i64;
        let mut width = 0i64;
        let mut buckets: Vec<i64> = Vec::new();
        let mut parts = seg.split_whitespace();
        if let Some(n) = parts.next() {
            name = Some(n.to_string());
        }
        for part in parts {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            match k {
                "count" => count = v.parse().unwrap_or(0),
                "total" => total = v.parse().unwrap_or(0),
                "min" => min = v.parse().unwrap_or(0),
                "max" => max = v.parse().unwrap_or(0),
                "lo" => lo = v.parse().unwrap_or(0),
                "width" => width = v.parse().unwrap_or(0),
                "buckets" => {
                    buckets = v
                        .split(',')
                        .filter_map(|x| x.parse().ok())
                        .collect();
                }
                _ => {}
            }
        }
        let Some(name) = name else {
            continue;
        };
        let buckets_json = JsonValue::Array(
            buckets
                .into_iter()
                .map(|b| JsonValue::Number(Number::from(b)))
                .collect(),
        );
        client
            .execute(
                r#"
                INSERT INTO just_drop_agg (
                    run_id, name, count, total_ticks, min_ticks, max_ticks, lo, width, buckets
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
                &[
                    &run_id,
                    &name,
                    &count,
                    &total,
                    &min,
                    &max,
                    &lo,
                    &width,
                    &buckets_json,
                ],
            )
            .await
            .context("INSERT just_drop_agg")?;
    }
    Ok(())
}

async fn ingest_gauge(client: &mut Client, run_id: i64, line: &str) -> Result<()> {
    let body = line
        .trim()
        .strip_prefix("OK gauge ")
        .context("expected OK gauge")?;
    for part in body.split_whitespace() {
        let Some((name, rest)) = part.split_once('=') else {
            continue;
        };
        let mut it = rest.split(',');
        let current: i64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let peak: i64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        client
            .execute(
                r#"
                INSERT INTO just_drop_gauge (run_id, name, current, peak)
                VALUES ($1, $2, $3, $4)
                "#,
                &[&run_id, &name, &current, &peak],
            )
            .await
            .context("INSERT just_drop_gauge")?;
    }
    Ok(())
}

async fn ingest_trace(client: &mut Client, run_id: i64, line: &str) -> Result<()> {
    let body = line.trim().strip_prefix("OK trace ").unwrap_or(line);
    let mut len = 0i64;
    let mut dropped = 0i64;
    for part in body.split_whitespace() {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "len" => len = v.parse().unwrap_or(0),
                "dropped" => dropped = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    client
        .execute(
            r#"
            INSERT INTO just_drop_trace_summary (run_id, len, dropped, kind_hist)
            VALUES ($1, $2, $3, $4)
            "#,
            &[&run_id, &len, &dropped, &json!({})],
        )
        .await
        .context("INSERT just_drop_trace_summary")?;
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

fn parse_drop_net_rx(line: &str) -> Result<(u64, u64)> {
    let map = counters::parse_counter_map(line)?;
    let drop = map
        .get("drop")
        .and_then(|v| v.as_i64())
        .context("missing drop=")? as u64;
    let net_rx = map
        .get("net_rx")
        .and_then(|v| v.as_i64())
        .context("missing net_rx=")? as u64;
    Ok((drop, net_rx))
}
