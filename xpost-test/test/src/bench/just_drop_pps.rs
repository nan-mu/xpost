//! `just_drop` — full-rate UDP blast, sample `drop` → `just_drop_config` / `just_drop_data`.
//!
//! Arms: `hal` (`just-drop-hal` + `drop on`) and `xdp` (`just-drop` profile includes xdp.elf).
//! Formal: DROP same-named tables then CREATE empty (`pg_dsn::prepare` + `TableSet::JustDrop`).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;
use serde_json::Value as JsonValue;
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

    let window = opts
        .window_secs
        .map(Duration::from_secs)
        .or_else(|| cfg.just_drop.window_secs.map(Duration::from_secs))
        .unwrap_or(if opts.smoke {
            PERF_SMOKE_WINDOW
        } else {
            PERF_WINDOW
        });
    let rounds = opts.rounds.unwrap_or(if opts.smoke {
        1
    } else {
        cfg.just_drop.rounds.max(1)
    });
    let sample = Duration::from_millis(opts.sample_interval_ms.max(100));
    let formal = opts.formal || cfg.just_drop.formal;

    RoundUi::banner(&format!(
        "just_drop arms={:?} agent={endpoint} esp={esp_s} window={}s sample={}ms rounds={rounds} formal={formal} pg={}",
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

    for _round in 1..=rounds {
        for path in &paths {
            run_arm(
                cfg,
                &agent,
                &ui,
                &mut eta,
                *path,
                window,
                sample,
                formal,
                first_round,
                pg.as_mut(),
            )
            .await
            .with_context(|| format!("arm={}", path.as_str()))?;
            first_round = false;
        }
    }

    RoundUi::banner(&format!(
        "just_drop done rounds={rounds} arms={}",
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
    window: Duration,
    sample: Duration,
    formal: bool,
    first_round: bool,
    pg: Option<&mut Client>,
) -> Result<()> {
    let profile = match path {
        PpsPath::Hal => cfg
            .profiles
            .just_drop_hal
            .as_deref()
            .unwrap_or("just-drop-hal"),
        PpsPath::Xdp => cfg.profiles.just_drop.as_deref().unwrap_or("just-drop"),
    };

    ui.begin_round();
    ui.set_eta(&eta.fmt());

    if first_round {
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

    // Round counters: after firmware ready → before next flash/reset.
    let counters_before = counters::fetch_counters(agent).await?;
    let started_at = SystemTime::now();

    let measure = measure_round(cfg, agent, ui, eta, path, window, sample).await;
    let (samples, drop_delta, net_rx_delta, host_pps, sent) = match measure {
        Ok(v) => v,
        Err(e) => {
            ui.clear_temp_for_error();
            return Err(e);
        }
    };
    let stopped_at = SystemTime::now();
    let counters_after = counters::fetch_counters(agent).await?;
    let counters_json = counters::counters_delta(&counters_before, &counters_after);

    ui.permanent(&format!(
        "    {}: dropΔ={drop_delta} net_rxΔ={net_rx_delta} host_pps={host_pps:.0} sent={sent} samples={}",
        path.as_str(),
        samples.len()
    ));

    eta.consume_fixed(window + Duration::from_millis(DRAIN_MS));
    ui.set_eta(&eta.fmt());

    if let Some(client) = pg {
        ui.check_start("写入 postgres");
        ingest_run(
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

    ui.end_round();
    Ok(())
}

async fn measure_round(
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

async fn ingest_run(
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
