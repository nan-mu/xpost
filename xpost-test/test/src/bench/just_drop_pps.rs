//! `just_drop` — formal exam: XDP/HAL CAG (full-rate + 800pps) + XDP Trace one-shot.
//!
//! Formal `window_s=30s`. No open-run DROP/TRUNCATE; decode report → ORM (no report_raw).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use rand::RngExt;

use crate::agent::AgentClient;
use crate::bench::counters;
use crate::bench::formal::{self, Firmware};
use crate::bench::pg_dsn::PgOpts;
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::bench::udp_blast;
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const WINDOW_S: Duration = Duration::from_secs(30);
const WINDOW_SMOKE: Duration = Duration::from_secs(10);
const DRAIN_MS: u64 = 200;
const FIXED_PPS: f64 = 800.0;

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

#[derive(Clone, Debug)]
struct Sample {
    t: SystemTime,
    drop: u64,
}

pub struct JustDropPpsOpts {
    pub paths: Vec<PpsPath>,
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub sample_interval_ms: u64,
    pub rounds: Option<usize>,
    pub formal_clear: bool,
}

fn profile_for(fw: Firmware) -> &'static str {
    match fw {
        Firmware::XdpCag => "just-drop-cag",
        Firmware::XdpT => "just-drop-t",
        Firmware::Hal => "just-drop-hal-cag",
    }
}

fn path_for(fw: Firmware) -> PpsPath {
    match fw {
        Firmware::Hal => PpsPath::Hal,
        _ => PpsPath::Xdp,
    }
}

pub async fn run_just_drop_pps(cfg: &FileConfig, opts: JustDropPpsOpts) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("just_drop needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint.clone());
    let esp_s = cfg.target.esp_ip.to_string();
    let formal = opts.formal || cfg.just_drop.formal;
    let window = opts
        .window_secs
        .map(Duration::from_secs)
        .unwrap_or(if opts.smoke { WINDOW_SMOKE } else { WINDOW_S });
    let sample = Duration::from_millis(opts.sample_interval_ms.max(1));

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
        "just_drop arms={:?} +xdp_t window={}s sample={}ms formal={formal} clear={} agent={endpoint} esp={esp_s} pg={}",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        window.as_secs(),
        sample.as_millis(),
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

    let mut eta = BudgetEta::new();
    eta.add_flashes(arms.len() + 1);
    // full-rate + 800pps per arm
    eta.add_fixed((READY_BUDGET + window * 2).saturating_mul(arms.len() as u32));
    eta.add_fixed(READY_BUDGET + Duration::from_secs(5));

    let ui = RoundUi::new();
    let exam_id = if let Some(client) = pg.as_mut() {
        ui.check_start("INSERT exam");
        let id = formal::insert_exam(client, Some("just_drop")).await?;
        ui.check_ok(&format!("exam={id}"));
        Some(id)
    } else {
        None
    };

    for fw in &arms {
        let profile = profile_for(*fw);
        let path = path_for(*fw);
        flash_and_arm(cfg, &agent, &ui, &mut eta, path, profile).await?;

        // Full-rate window with drop sampling.
        agent.perf_clear().await.context("perf_clear")?;
        let counters_before = counters::fetch_counters(&agent).await?;
        let started_at = SystemTime::now();
        let (samples, drop_delta, net_rx_delta, host_pps, sent) =
            measure_counter_round(cfg, &agent, &ui, &eta, path, window, sample).await?;
        let stopped_at = SystemTime::now();
        let _ = counters::counters_delta(&counters_before, &counters::fetch_counters(&agent).await?);
        ui.permanent(&format!(
            "    {}/full: dropΔ={drop_delta} net_rxΔ={net_rx_delta} host_pps={host_pps:.0} sent={sent}",
            fw.as_db()
        ));
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 full-rate");
            let cfg_id = formal::insert_just_drop_config(
                client,
                exam_id,
                *fw,
                host_pps,
                window.as_secs_f64(),
                Some(sample.as_millis() as i32),
                started_at,
                stopped_at,
                sent as i64,
            )
            .await?;
            let rows: Vec<(SystemTime, i64)> = samples
                .iter()
                .map(|s| (s.t, s.drop as i64))
                .collect();
            formal::insert_just_drop_data(client, cfg_id, &rows).await?;
            let msg = agent.request("report").await.context("report")?;
            let decoded = formal::decode_report_msg(&msg)?;
            formal::write_perf(client, exam_id, cfg_id, *fw, &decoded).await?;
            ui.check_ok(&format!("config={cfg_id} mask={:#x}", decoded.mask));
        }

        // Fixed 800 pps window.
        agent.perf_clear().await.context("perf_clear")?;
        let started_at = SystemTime::now();
        let sent = paced_blast(cfg, &ui, &eta, path, window, FIXED_PPS).await?;
        tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
        let stopped_at = SystemTime::now();
        ui.permanent(&format!(
            "    {}/800pps: sent={sent}",
            fw.as_db()
        ));
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 800pps");
            let cfg_id = formal::insert_just_drop_config(
                client,
                exam_id,
                *fw,
                FIXED_PPS,
                window.as_secs_f64(),
                None,
                started_at,
                stopped_at,
                sent as i64,
            )
            .await?;
            let msg = agent.request("report").await.context("report")?;
            let decoded = formal::decode_report_msg(&msg)?;
            formal::write_perf(client, exam_id, cfg_id, *fw, &decoded).await?;
            ui.check_ok(&format!("config={cfg_id} mask={:#x}", decoded.mask));
        }
    }

    // Trace one-shot.
    {
        let fw = Firmware::XdpT;
        let profile = profile_for(fw);
        flash_and_arm(cfg, &agent, &ui, &mut eta, PpsPath::Xdp, profile).await?;
        agent.perf_clear().await.context("perf_clear")?;
        let started_at = SystemTime::now();
        let sent = paced_blast(cfg, &ui, &eta, PpsPath::Xdp, Duration::from_millis(50), 20.0).await?;
        tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
        let stopped_at = SystemTime::now();
        if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
            ui.check_start("写入 trace");
            let cfg_id = formal::insert_just_drop_config(
                client,
                exam_id,
                fw,
                1.0,
                0.0,
                None,
                started_at,
                stopped_at,
                sent.max(1) as i64,
            )
            .await?;
            let msg = agent.request("report").await.context("report")?;
            let decoded = formal::decode_report_msg(&msg)?;
            formal::write_perf(client, exam_id, cfg_id, fw, &decoded).await?;
            let n_ev = decoded.trace.as_ref().map(|t| t.events.len()).unwrap_or(0);
            ui.check_ok(&format!("trace events={n_ev}"));
        }
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        formal::stop_exam(client, exam_id).await?;
    }
    RoundUi::banner(&format!("just_drop done arms={}", arms.len()));
    Ok(())
}

async fn flash_and_arm(
    cfg: &FileConfig,
    agent: &AgentClient,
    ui: &RoundUi,
    eta: &mut BudgetEta,
    path: PpsPath,
    profile: &str,
) -> Result<()> {
    ui.begin_round();
    ui.set_eta(&eta.fmt());

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


pub(crate) async fn paced_blast(
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
