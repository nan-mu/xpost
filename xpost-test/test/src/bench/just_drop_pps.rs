//! Formal `just_drop` — Array Map vs native array overhead (PPS grid).
//!
//! 目的：测量 DUT 上 **array map 读写** 的耗时（相对 native array / HAL 对照）。
//!
//! ```text
//! just_drop:
//!   for fw in {hal, xdp_cag}:
//!     agent flash fw
//!     for rate in {400, 800, …, 4000}:
//!       agent clear perf
//!       agent clear map(counter)
//!       agent enable xdp
//!       sent rate 30s
//!       agent disable xdp
//!       agent get map(counter)
//!       agent get perf
//!       upload to db
//!
//!   agent flash xdp_t
//!   agent clear perf
//!   sent 1
//!   agent get perf
//!   upload to db
//! ```
//!
//! # 数据处理
//!
//! 每格写入 `just_drop_config` + `just_drop_data`（DDL：`sql/just_drop.sql`）。
//! Perf（`agent get perf`）另落共享 / HAL CAG 表，不在此列。
//!
//! `just_drop_config`：
//! - `exam_id` — 本轮 `exam` 行
//! - `firmware` — 当前 `fw`（`hal` / `xdp_cag` / `xdp_t`）
//! - `target_pps` — 网格 `rate`（Trace 臂为占位）
//! - `window_s` — 刺激时长（网格 30；Trace 臂为 0）
//! - `source_seed` — 多源 stim 的确定性种子（末字节分桶）
//! - `started_at` / `stopped_at` — 该格刺激墙钟起止
//! - `sent` — 主机实际发出的包数
//!
//! `just_drop_data`（每 slot 一行，0..255）：
//! - `config_id` — 所属 `just_drop_config`
//! - `slot` — IPv4 源地址末字节（分桶下标）
//! - `value` — 该格结束后 `agent get map(counter)` 读到的槽值（清零后刺激，故为本格增量）

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};

use crate::agent::AgentClient;
use crate::bench::formal::{self, Firmware};
use crate::bench::pg_dsn::PgOpts;
use crate::bench::status::{self, BudgetEta, READY_BUDGET, RoundUi};
use crate::config::FileConfig;
use crate::raw_tx::{RawTx, iface_mac};

const STIM_PORT: u16 = 8080;
const FLASH_IDLE: Duration = Duration::from_secs(300);
const WINDOW_S: Duration = Duration::from_secs(30);
const WINDOW_SMOKE: Duration = Duration::from_secs(4);
const DRAIN_MS: u64 = 200;
const MAP_NAME: &str = "counter";
const SOURCE_SEED_DEFAULT: i64 = 0x0A11_CE00;
/// Absolute slot slack for Wi-Fi loss (WARN only).
const SLOT_SLACK: u64 = 64;

const SWEEP_PPS: &[f64] = &[
    400.0, 800.0, 1200.0, 1600.0, 2000.0, 2400.0, 2800.0, 3200.0, 3600.0, 4000.0,
];
const SWEEP_SMOKE_PPS: &[f64] = &[400.0, 1200.0, 2400.0, 4000.0];

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

/// Deterministic stim src IP: last octet cycles with seed; avoids DUT own octet.
pub fn stim_src_ip(seed: i64, pkt_i: u64, esp: Ipv4Addr) -> Ipv4Addr {
    let o = esp.octets();
    let mut octet = ((seed as u64).wrapping_add(pkt_i) % 256) as u8;
    if octet == o[3] {
        octet = octet.wrapping_add(1);
    }
    Ipv4Addr::new(o[0], o[1], o[2], octet)
}

pub fn expected_buckets(seed: i64, sent: u64, esp: Ipv4Addr) -> [u64; 256] {
    let mut out = [0u64; 256];
    for i in 0..sent {
        let octet = stim_src_ip(seed, i, esp).octets()[3] as usize;
        out[octet] = out[octet].saturating_add(1);
    }
    out
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
    let grid: &[f64] = if opts.smoke { SWEEP_SMOKE_PPS } else { SWEEP_PPS };
    let source_seed = SOURCE_SEED_DEFAULT;
    let _ = opts.sample_interval_ms;
    let _ = opts.rounds;

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
        "just_drop array arms={:?} +xdp_t grid={:?} window={}s seed={source_seed:#x} formal={formal} clear={} agent={endpoint} esp={esp_s} pg={}",
        arms.iter().map(|a| a.as_db()).collect::<Vec<_>>(),
        grid.iter().map(|p| *p as u32).collect::<Vec<_>>(),
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

    let mut eta = BudgetEta::new();
    eta.add_flashes(arms.len() + 1);
    eta.add_fixed(
        (READY_BUDGET + window)
            .saturating_mul((arms.len() * grid.len()) as u32),
    );
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

    let esp = cfg.target.esp_ip;

    for fw in &arms {
        let profile = profile_for(*fw);
        let path = path_for(*fw);
        flash_and_arm(cfg, &agent, &ui, &mut eta, path, profile).await?;

        for &pps in grid {
            agent.perf_clear().await.context("perf_clear")?;
            // Clear array state via remount is flash-only; soft-clear not available —
            // dump before window and subtract for correctness, or rely on fresh flash.
            // Per-window: flash once per arm, so counters accumulate across pps.
            // Plan wants per-window state: re-flash is expensive; instead clear by
            // recording before snapshot and storing delta in just_drop_data.
            let before = dump_slots(&agent).await.context("map dump before")?;
            let started_at = SystemTime::now();
            let sent = paced_multi_src(cfg, &ui, &eta, path, window, pps, source_seed).await?;
            tokio::time::sleep(Duration::from_millis(DRAIN_MS)).await;
            let stopped_at = SystemTime::now();
            let after = dump_slots(&agent).await.context("map dump after")?;
            let delta = slot_delta(&before, &after);
            let sum: u64 = delta.iter().map(|&v| v as u64).sum();
            if sum == 0 {
                bail!(
                    "{} @{pps}: map dump counter sumΔ=0 after sent={sent}",
                    fw.as_db()
                );
            }
            warn_correctness(source_seed, sent, esp, &delta, fw.as_db(), pps);

            ui.permanent(&format!(
                "    {} @{pps:.0}: sent={sent} mapΔ={sum}",
                fw.as_db()
            ));

            if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
                ui.check_start(&format!("写入 @{pps:.0}"));
                let cfg_id = formal::insert_just_drop_config(
                    client,
                    exam_id,
                    *fw,
                    pps,
                    window.as_secs_f64(),
                    source_seed,
                    started_at,
                    stopped_at,
                    sent as i64,
                )
                .await?;
                let rows: Vec<(i32, i64)> = delta
                    .iter()
                    .enumerate()
                    .map(|(slot, &v)| (slot as i32, v))
                    .collect();
                formal::insert_just_drop_data(client, cfg_id, &rows).await?;
                let msg = agent.request("report").await.context("report")?;
                let decoded = formal::decode_report_msg(&msg)?;
                formal::write_perf(
                    client,
                    exam_id,
                    cfg_id,
                    *fw,
                    formal::PerfExperiment::JustDrop,
                    &decoded,
                )
                .await?;
                ui.check_ok(&format!("config={cfg_id} mask={:#x}", decoded.mask));
            }
            eta.consume_fixed(window);
            ui.set_eta(&eta.fmt());
        }
    }

    // Trace one-shot.
    {
        let fw = Firmware::XdpT;
        let profile = profile_for(fw);
        flash_and_arm(cfg, &agent, &ui, &mut eta, PpsPath::Xdp, profile).await?;
        agent.perf_clear().await.context("perf_clear")?;
        let started_at = SystemTime::now();
        let sent = send_one(cfg, source_seed).await?;
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
                source_seed,
                started_at,
                stopped_at,
                sent as i64,
            )
            .await?;
            let msg = agent.request("report").await.context("report")?;
            let decoded = formal::decode_report_msg(&msg)?;
            formal::write_perf(
                client,
                exam_id,
                cfg_id,
                fw,
                formal::PerfExperiment::JustDrop,
                &decoded,
            )
            .await?;
            let n_ev = decoded.trace.as_ref().map(|t| t.events.len()).unwrap_or(0);
            ui.check_ok(&format!("trace events={n_ev}"));
        }
    }

    if let (Some(client), Some(exam_id)) = (pg.as_mut(), exam_id) {
        formal::stop_exam(client, exam_id).await?;
    }
    RoundUi::banner(&format!(
        "just_drop done arms={} grid={}",
        arms.len(),
        grid.len()
    ));
    Ok(())
}

fn warn_correctness(
    seed: i64,
    sent: u64,
    esp: Ipv4Addr,
    actual_delta: &[i64; 256],
    arm: &str,
    pps: f64,
) {
    let expected = expected_buckets(seed, sent, esp);
    let mut bad = 0u32;
    for (slot, &got) in actual_delta.iter().enumerate() {
        let exp = expected[slot] as i64;
        let diff = (got - exp).unsigned_abs();
        if diff > SLOT_SLACK {
            bad += 1;
        }
    }
    if bad > 0 {
        eprintln!(
            "    WARN: {arm} @{pps:.0} correctness {bad}/256 slots beyond slack={SLOT_SLACK} (Wi-Fi loss expected at high pps)"
        );
    }
}

async fn dump_slots(agent: &AgentClient) -> Result<[i64; 256]> {
    let raw = agent.map_dump(MAP_NAME).await?;
    parse_slots(&raw)
}

fn parse_slots(dump: &str) -> Result<[i64; 256]> {
    let dump = dump.trim();
    if dump.starts_with("ERR ") {
        bail!("map dump error: {dump}");
    }
    let mut out = [0i64; 256];
    let mut seen = 0u32;
    for line in dump.lines() {
        if line.starts_with("OK map ") || line.eq_ignore_ascii_case("OK map end") {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(slot_s) = parts.next() else {
            continue;
        };
        let Some(val_s) = parts.next() else {
            continue;
        };
        if parts.next().is_some() {
            continue;
        }
        let Ok(slot) = slot_s.parse::<usize>() else {
            continue;
        };
        let Ok(value) = val_s.parse::<i64>() else {
            continue;
        };
        if slot < 256 {
            out[slot] = value;
            seen += 1;
        }
    }
    if seen == 0 {
        bail!("map dump had no slot lines");
    }
    Ok(out)
}

fn slot_delta(before: &[i64; 256], after: &[i64; 256]) -> [i64; 256] {
    let mut d = [0i64; 256];
    for i in 0..256 {
        d[i] = after[i].saturating_sub(before[i]);
    }
    d
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
    let _ = cfg;
    Ok(())
}

async fn paced_multi_src(
    cfg: &FileConfig,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    target_pps: f64,
    seed: i64,
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
        let spoof = stim_src_ip(seed, sent, esp);
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

/// Trace arm: one multi-src-shaped UDP to DUT echo port.
async fn send_one(cfg: &FileConfig, seed: i64) -> Result<u64> {
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
    let spoof = stim_src_ip(seed, 0, esp);
    raw.send_udp(spoof, esp, STIM_PORT, b"D")
        .context("send_one")?;
    Ok(1)
}

/// Shared by `just_tx` flame helper (legacy import).
pub(crate) async fn paced_blast(
    cfg: &FileConfig,
    ui: &RoundUi,
    eta: &BudgetEta,
    path: PpsPath,
    window: Duration,
    target_pps: f64,
) -> Result<u64> {
    paced_multi_src(cfg, ui, eta, path, window, target_pps, SOURCE_SEED_DEFAULT).await
}
