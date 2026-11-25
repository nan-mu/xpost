//! Collect one medium network cell after firmware preparation.
//!
//! Reset policy inputs before each attempt. Arm perf after policy enable.
//! Send the planned stimulus. Wait for the drain interval.
//! Freeze the session before you disable the policy.
//! Read the export. Release the epoch before you write to the database.

#[path = "medium_network_capture.rs"]
mod capture;
#[path = "medium_network_store.rs"]
mod store;

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::time::{sleep, timeout};
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    agent::{AgentClient, PerfFreeze},
    config::FileConfig,
    db::{metrics, protocol},
    pipeline::{self, RawTx},
};

const RATES: [i32; 3] = [400, 1400, 4000];
const WINDOW: Duration = Duration::from_secs(5);
const DRAIN: Duration = Duration::from_millis(250);
// Allow the final tcpdump capture block to retire after its 1 s buffer timeout.
const CAPTURE_TAIL: Duration = Duration::from_millis(1_200);
const BODY: usize = 40;
const PORT: u16 = 8080;
const LEGAL_EVERY: i64 = 10;
const ANALYSIS_VERSION: &str = "medium-network-v2";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Workload {
    Firewall,
    Drop,
    Tx,
}

impl Workload {
    fn parse(experiment: &str) -> Result<Self> {
        match experiment {
            "firewall" => Ok(Self::Firewall),
            "just_drop" => Ok(Self::Drop),
            "just_tx" => Ok(Self::Tx),
            _ => bail!("unknown medium network experiment {experiment}"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Firewall => "firewall",
            Self::Drop => "just_drop",
            Self::Tx => "just_tx",
        }
    }

    fn toggles_policy(self, mode: &str) -> bool {
        mode != "hal" || self == Self::Drop
    }
}

#[derive(Clone, Copy)]
enum Stimulus {
    Rate(i32),
    Hit,
    Miss,
    Tagged,
}

impl Stimulus {
    fn rate(self) -> Option<i32> {
        match self {
            Self::Rate(rate) => Some(rate),
            _ => None,
        }
    }

    fn case(self) -> &'static str {
        match self {
            Self::Rate(_) => "mixed",
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Tagged => "tagged",
        }
    }

    fn legal(self, seq: i64) -> bool {
        match self {
            Self::Rate(_) => seq % LEGAL_EVERY == 0,
            Self::Miss => true,
            _ => false,
        }
    }
}

struct Cell<'a> {
    experiment: Workload,
    mode: &'a str,
    variant: &'a str,
    firmware: &'a str,
    firmware_sha256: &'a str,
}

impl Cell<'_> {
    fn hal(&self) -> bool {
        self.mode == "hal"
    }

    fn toggles_policy(&self) -> bool {
        self.experiment.toggles_policy(self.mode)
    }
}

pub(super) fn planned_input(
    cfg: &FileConfig,
    experiment: &str,
    condition: &str,
) -> Result<serde_json::Value> {
    let link = Link::from_config(cfg)?;
    let rules = if experiment == "firewall" {
        link.rules(cfg.firewall.blacklist_size, cfg.target.vip_ip)?
    } else {
        Vec::new()
    };
    Ok(json!({
        "condition": condition,
        "window_ns": WINDOW.as_nanos() as u64,
        "drain_ns": DRAIN.as_nanos() as u64,
        "source_ip": link.src, "dut_ip": link.dut, "interface": link.iface,
        "source_mac": link.src_mac, "dut_mac": link.dut_mac,
        "port": link.port, "stimulus_source_port": link.sport,
        "firewall_hit": link.hit, "blacklist": rules,
    }))
}

/// Run a cell for the firmware and XDP image already prepared by the scheduler.
/// Individual attempts print their run ID and terminal status.
pub(super) async fn run_cell(
    cfg: &FileConfig,
    agent: &AgentClient,
    client: &mut Client,
    experiment: &str,
    mode: &str,
    variant: &str,
    firmware: &str,
    firmware_sha256: &str,
    campaign: Uuid,
    plan_order: &mut i32,
    resume: &super::medium::Resume,
) -> Result<()> {
    let experiment = Workload::parse(experiment)?;
    ensure!(
        matches!(mode, "hal" | "aot" | "jit" | "rbpf"),
        "invalid mode {mode}"
    );
    let plan: Vec<_> = match variant {
        "metrics" => RATES
            .into_iter()
            .flat_map(|rate| [(Stimulus::Rate(rate), 1), (Stimulus::Rate(rate), 2)])
            .collect(),
        "trace" if experiment == Workload::Firewall => {
            vec![(Stimulus::Hit, 1), (Stimulus::Miss, 1)]
        }
        "trace" => vec![(Stimulus::Tagged, 1)],
        _ => bail!("invalid collection variant {variant}"),
    };
    let cell = Cell {
        experiment,
        mode,
        variant,
        firmware,
        firmware_sha256,
    };
    let profile: [u8; 32] = Sha256::digest(serde_json::to_vec(&(ANALYSIS_VERSION, &cell))?).into();
    let link = Link::from_config(cfg)?;
    let rules = match experiment {
        Workload::Firewall => link.rules(cfg.firewall.blacklist_size, cfg.target.vip_ip)?,
        _ => Vec::new(),
    };
    for (stimulus, repeat) in plan {
        let condition = stimulus
            .rate()
            .map_or_else(|| stimulus.case().to_owned(), |rate| rate.to_string());
        if !resume.should_run(experiment.name(), mode, variant, &condition, repeat) {
            continue;
        }
        let retry_of = resume.retry_of(experiment.name(), mode, variant, &condition, repeat);
        let run = Uuid::new_v4();
        let order = *plan_order;
        *plan_order = order.checked_add(1).context("medium plan order overflow")?;
        let parameters = json!({
            "medium_id": campaign.to_string(),
            "stage": crate::config::MediumStage::for_mode(mode),
            "medium_identity": resume.identity(experiment.name(), mode, variant, &condition, repeat),
            "retry_of": retry_of.map(|run| run.to_string()),
            "firmware": firmware,
            "firmware_sha256": firmware_sha256,
            "port": link.port,
            "stimulus_source_port": link.sport,
            "source_ip": link.src,
            "dut_ip": link.dut,
            "interface": link.iface,
            "blacklist": rules,
            "pps": stimulus.rate(),
            "probe": stimulus.case(),
            "window_ns": stimulus.rate().map(|_| WINDOW.as_nanos() as u64),
            "drain_ns": DRAIN.as_nanos() as u64,
        });
        let attempt = match store::Attempt::start(
            client,
            &cell,
            &link,
            run,
            order,
            repeat,
            stimulus,
            &profile,
            &parameters,
            &rules,
        )
        .await
        {
            Ok(attempt) => attempt,
            Err(error) => {
                println!(
                    "XPOST_RESULT {}",
                    json!({
                        "medium_id": campaign.to_string(), "run_id": run.to_string(), "mode": mode, "experiment": experiment.name(),
                        "variant": variant, "condition": parameters, "repeat": repeat, "plan_order": order,
                        "status": "database_error", "error": format!("{error:#}"),
                    })
                );
                return Err(error).with_context(|| format!("start network attempt {run}"));
            }
        };
        let mut observation = Observation::default();
        if let Err(error) = collect(
            agent,
            &cell,
            &link,
            &rules,
            run,
            stimulus,
            &profile,
            &mut observation,
        )
        .await
        {
            observation.fail(format!("{error:#}"));
        }
        let result = analyze(
            &cell,
            &link,
            &rules,
            run,
            stimulus,
            &profile,
            &mut observation,
        );
        let summary = result.summary(&observation);
        let intended_status = observation.status(&result);
        let (status, recovered) = match attempt
            .finish_recovering(
                client,
                &cfg.postgres,
                &cell,
                &link,
                &observation,
                &result,
                intended_status,
            )
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let observed = protocol::run_status(&cfg.postgres, run).await;
                let recovery =
                    store::recover(run, &observation, &summary).map(|p| p.display().to_string());
                println!(
                    "XPOST_RESULT {}",
                    json!({
                        "medium_id": campaign.to_string(), "run_id": run.to_string(), "mode": mode, "experiment": experiment.name(), "variant": variant,
                        "condition": parameters, "repeat": repeat, "plan_order": order, "status": "database_error",
                        "database_status": observed, "error": format!("{error:#}"),
                        "evidence": recovery.as_ref().ok(), "evidence_error": recovery.err().map(|error| error.to_string()),
                        "result": summary,
                    })
                );
                return Err(error).context("persist network attempt");
            }
        };
        println!(
            "XPOST_RESULT {}",
            json!({
                "medium_id": campaign.to_string(), "run_id": run.to_string(), "mode": mode, "experiment": experiment.name(), "variant": variant,
                "condition": parameters, "repeat": repeat, "plan_order": order, "status": status,
                "issues": observation.issues, "result": summary, "database_recovered": recovered,
            })
        );
        ensure!(
            !observation.unsafe_to_continue,
            "network session or policy cleanup failed for {run}"
        );
    }
    Ok(())
}

impl Serialize for Cell<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        (
            self.experiment.name(),
            self.mode,
            self.variant,
            self.firmware,
            self.firmware_sha256,
        )
            .serialize(serializer)
    }
}

#[derive(Clone)]
struct Link {
    iface: String,
    src: Ipv4Addr,
    dut: Ipv4Addr,
    hit: Ipv4Addr,
    src_mac: [u8; 6],
    dut_mac: [u8; 6],
    port: u16,
    sport: u16,
}

impl Link {
    fn from_config(cfg: &FileConfig) -> Result<Self> {
        let iface = cfg.probe.iface.clone();
        let src_mac = match cfg.src_mac_override()? {
            Some(mac) => mac,
            None => pipeline::iface_mac(&iface)?,
        };
        let src = pipeline::iface_ipv4(&iface)?;
        let dut = cfg.target.esp_ip;
        let [a, b, c, _] = dut.octets();
        let hit = (2..255)
            .map(|n| Ipv4Addr::new(a, b, c, n))
            .find(|ip| ![src, dut, cfg.target.vip_ip].contains(ip))
            .context("no firewall hit source")?;
        ensure!(
            cfg.target.echo_port == PORT,
            "medium network firmware listens on port {PORT}"
        );
        ensure!(
            cfg.target.stim_sport != 0 && cfg.target.stim_sport != PORT,
            "invalid stimulus source port"
        );
        let dut_mac = cfg.esp_mac_bytes()?;
        ensure!(
            src_mac != dut_mac && src != dut,
            "DUT and host identities must differ"
        );
        Ok(Self {
            iface,
            src,
            dut,
            hit,
            src_mac,
            dut_mac,
            port: PORT,
            sport: cfg.target.stim_sport,
        })
    }

    fn rules(&self, count: u32, vip: Ipv4Addr) -> Result<Vec<Ipv4Addr>> {
        let [a, b, c, _] = self.dut.octets();
        let rules = (2..255)
            .map(|n| Ipv4Addr::new(a, b, c, n))
            .filter(|ip| ![self.src, self.dut, vip].contains(ip))
            .take(count as usize)
            .collect::<Vec<_>>();
        ensure!(
            count > 0 && rules.len() == count as usize,
            "blacklist size does not fit controlled addresses"
        );
        Ok(rules)
    }

    fn open(&self) -> Result<RawTx> {
        RawTx::open(
            &self.iface,
            self.src_mac,
            self.dut_mac,
            self.dut,
            self.port,
            self.sport,
        )
    }
}

type Counters = BTreeMap<Ipv4Addr, u64>;

#[derive(Serialize)]
struct Send {
    seq: i64,
    mono: u64,
    wall: u64,
    submitted: bool,
    error: Option<String>,
    class: &'static str,
    source: Ipv4Addr,
    payload: Vec<u8>,
}

#[derive(Default)]
struct Observation {
    issues: Vec<String>,
    failed: bool,
    unsafe_to_continue: bool,
    boot: Option<u64>,
    epoch: Option<u32>,
    arm: Option<u64>,
    freeze: Option<PerfFreeze>,
    perf: Option<Vec<u8>>,
    capture: Option<capture::Evidence>,
    sends: Vec<Send>,
    elapsed: Option<u64>,
    drain_mono: Option<u64>,
    drain_wall: Option<u64>,
    maps_before: Option<Counters>,
    maps_after: Option<Counters>,
    tokens: Option<Vec<u16>>,
    refused: Option<u64>,
    rx_before: Option<u64>,
    rx_after: Option<u64>,
}

impl Observation {
    fn fail(&mut self, issue: String) {
        self.issues.push(issue);
        self.failed = true;
    }

    fn status(&self, analysis: &Analysis) -> &'static str {
        if self.failed {
            "failed"
        } else if analysis.qualified {
            "complete"
        } else {
            "inconclusive"
        }
    }
}

async fn collect(
    agent: &AgentClient,
    cell: &Cell<'_>,
    link: &Link,
    rules: &[Ipv4Addr],
    run: Uuid,
    stimulus: Stimulus,
    profile: &[u8; 32],
    out: &mut Observation,
) -> Result<()> {
    let raw = link.open()?;
    let _tx_sink = if cell.experiment == Workload::Tx && cell.hal() {
        Some(
            tokio::net::UdpSocket::bind((link.src, link.sport))
                .await
                .context("bind HAL echo port")?,
        )
    } else {
        None
    };
    if cell.toggles_policy() {
        agent
            .disable_xdp()
            .await
            .context("prepare with policy disabled")?;
    }
    if cell.experiment == Workload::Drop {
        raw.announce_neighbor(link.src)
            .context("prime DUT neighbor")?;
        qualify_drop_path(link).await?;
    } else if cell.experiment == Workload::Tx && cell.hal() {
        raw.announce_neighbor(link.src)
            .context("prime HAL echo neighbor")?;
    }
    if cell.experiment == Workload::Firewall {
        agent.clear_map("blacklist").await?;
        agent.update_map_ips("blacklist", rules).await?;
        agent.clear_map("firewall_result").await?;
        out.maps_before = Some(blacklist(agent).await?);
        ensure!(
            out.maps_before
                .as_ref()
                .is_some_and(|baseline| baseline.keys().copied().eq(rules.iter().copied())),
            "DUT blacklist differs from the fixture"
        );
    }
    let mut capture = if cell.experiment == Workload::Tx {
        Some(capture::Capture::start(link)?)
    } else {
        None
    };
    let result = capture_window(
        agent,
        cell,
        link,
        raw,
        run,
        stimulus,
        profile,
        out,
        &mut capture,
    )
    .await;
    if let Some(capture) = capture {
        out.capture = Some(capture.stop().await);
    }
    result
}

async fn capture_window(
    agent: &AgentClient,
    cell: &Cell<'_>,
    link: &Link,
    raw: RawTx,
    run: Uuid,
    stimulus: Stimulus,
    profile: &[u8; 32],
    out: &mut Observation,
    capture: &mut Option<capture::Capture>,
) -> Result<()> {
    if let Some(capture) = capture {
        capture.ready().await?;
    }
    if cell.experiment == Workload::Tx {
        out.rx_before = Some(
            agent
                .map_slot("rxcnt", 17)
                .await
                .context("read initial rxcnt")?,
        );
    }
    let (epoch, boot) = match agent
        .perf_prepare(run, &protocol::hex(profile), cell.variant)
        .await
    {
        Ok(prepared) => prepared,
        Err(error) => {
            let detail = format!("{error:#}");
            out.unsafe_to_continue =
                !detail.contains("perf unsupported") && !detail.contains("perf unavailable");
            return Err(error).context("perf prepare");
        }
    };
    out.epoch = Some(epoch);
    out.boot = Some(boot);

    let window = async {
        if cell.toggles_policy() {
            agent.enable_xdp().await.context("enable policy")?;
        }
        out.arm = Some(agent.perf_arm(epoch).await.context("perf arm")?);
        let link = link.clone();
        let firewall = cell.experiment == Workload::Firewall;
        let (sends, elapsed, start) =
            tokio::task::spawn_blocking(move || send(raw, &link, run, stimulus, firewall))
                .await
                .context("join network sender")??;
        out.sends = sends;
        out.elapsed = Some(elapsed);
        sleep(DRAIN).await;
        out.drain_mono = Some(start.elapsed().as_nanos() as u64);
        out.drain_wall = Some(wall_ns()?);
        Ok::<_, anyhow::Error>(())
    }
    .await;

    let window_succeeded = window.is_ok();
    let capture_stop = capture.take().map(|capture| {
        tokio::spawn(async move {
            if window_succeeded {
                sleep(CAPTURE_TAIL).await;
            }
            capture.stop().await
        })
    });

    // An arm failure can leave the session prepared or armed.
    if out.arm.is_none() {
        let _ = agent.perf_arm(epoch).await;
    }
    match agent.perf_freeze(epoch).await {
        Ok(freeze) => out.freeze = Some(freeze),
        Err(error) => {
            out.fail(format!("perf freeze: {error:#}"));
            out.unsafe_to_continue = true;
        }
    }
    if cell.toggles_policy() {
        if let Err(error) = agent.disable_xdp().await {
            out.fail(format!("disable policy after freeze: {error:#}"));
            out.unsafe_to_continue = true;
        }
    }

    if let Some(freeze) = out.freeze {
        let post = async {
            if cell.experiment == Workload::Firewall {
                out.maps_after = Some(blacklist(agent).await?);
                out.tokens = Some(
                    agent
                        .get_map("firewall_result")
                        .await?
                        .entries
                        .iter()
                        .map(|entry| entry.value.parse().context("decode delivered u16 token"))
                        .collect::<Result<Vec<u16>>>()?,
                );
                out.refused = Some(agent.map_slot("firewall_dropped", 0).await?);
            } else if cell.experiment == Workload::Tx {
                out.rx_after = Some(
                    agent
                        .map_slot("rxcnt", 17)
                        .await
                        .context("read final rxcnt")?,
                );
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = post {
            out.fail(format!("read policy evidence: {error:#}"));
        }
        match agent.perf_read(epoch, freeze).await {
            Ok(bytes) => out.perf = Some(bytes),
            Err(error) => out.fail(format!("perf read: {error:#}")),
        }
        if let Err(error) = agent.perf_release(epoch).await {
            out.fail(format!("perf release: {error:#}"));
            out.unsafe_to_continue = true;
        }
    }
    if let Some(stop) = capture_stop {
        match stop.await {
            Ok(evidence) => out.capture = Some(evidence),
            Err(error) => out.fail(format!("join capture: {error:#}")),
        }
    }
    window
}

pub(super) async fn qualify_group(
    cfg: &FileConfig,
    agent: &AgentClient,
    experiment: &str,
    mode: &str,
) -> Result<()> {
    let link = Link::from_config(cfg)?;
    if Workload::parse(experiment)?.toggles_policy(mode) {
        agent
            .disable_xdp()
            .await
            .context("disable policy for dataplane readiness")?;
    }
    link.open()?
        .announce_neighbor(link.src)
        .context("prime DUT neighbor")?;
    qualify_drop_path(&link).await
}

async fn qualify_drop_path(link: &Link) -> Result<()> {
    let socket = tokio::net::UdpSocket::bind((link.src, 0))
        .await
        .context("bind dataplane readiness socket")?;
    socket.connect((link.dut, link.port)).await?;
    timeout(Duration::from_secs(8), async {
        let mut bytes = [0u8; 32];
        for _ in 0..16 {
            socket.send(b"READY").await?;
            if let Ok(received) = timeout(Duration::from_millis(400), socket.recv(&mut bytes)).await
            {
                let size = received?;
                ensure!(
                    &bytes[..size] == b"ACK\n",
                    "unexpected dataplane readiness reply"
                );
                return Ok(());
            }
        }
        bail!("no DUT dataplane readiness ACK")
    })
    .await
    .context("DUT dataplane readiness deadline")?
}

async fn blacklist(agent: &AgentClient) -> Result<Counters> {
    let map = agent.get_map("blacklist").await?;
    let mut counters = BTreeMap::new();
    for entry in map.entries {
        ensure!(
            counters
                .insert(entry.key.parse()?, entry.value.parse()?)
                .is_none(),
            "duplicate blacklist key"
        );
    }
    Ok(counters)
}

fn payload(run: Uuid, seq: i64, token: Option<u16>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(BODY + usize::from(token.is_some()) * 2);
    if let Some(token) = token {
        bytes.extend_from_slice(&token.to_le_bytes());
    }
    bytes.extend_from_slice(b"JTX1\x01\0\0\x20");
    bytes.extend_from_slice(run.as_bytes());
    bytes.extend_from_slice(&(seq as u64).to_be_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes
}

fn token(run: Uuid, seq: i64) -> u16 {
    let salt = u16::from_le_bytes(run.as_bytes()[..2].try_into().unwrap()) as i64;
    ((salt + seq - 1) % 65_535 + 1) as u16
}

fn wall_ns() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

fn send(
    raw: RawTx,
    link: &Link,
    run: Uuid,
    stimulus: Stimulus,
    firewall: bool,
) -> Result<(Vec<Send>, u64, Instant)> {
    let start = Instant::now();
    let rate = stimulus.rate().map(|rate| rate as i64);
    let slots = rate.map_or(1, |rate| rate * WINDOW.as_secs() as i64);
    let mut sends = Vec::with_capacity(slots as usize);
    for slot in 0..slots {
        if let Some(rate) = rate {
            let target = start + Duration::from_nanos((slot * 1_000_000_000 / rate) as u64);
            wait_slot(target);
            let limit = target + Duration::from_nanos(1_000_000_000 / rate as u64);
            if Instant::now() >= limit {
                continue;
            }
        }
        let seq = slot + 1;
        let (source, class, token) = if firewall {
            let value = Some(token(run, seq));
            if stimulus.legal(seq) {
                (link.src, "legal", value)
            } else {
                (link.hit, "illegal", value)
            }
        } else {
            (link.src, "probe", None)
        };
        let payload = payload(run, seq, token);
        let mono = start.elapsed().as_nanos() as u64;
        let wall = wall_ns()?;
        let status = raw.send_udp(source, link.dut, link.port, &payload);
        sends.push(Send {
            seq,
            mono,
            wall,
            submitted: status.is_ok(),
            error: status.err().map(|e| format!("{e:#}")),
            class,
            source,
            payload,
        });
    }
    if rate.is_some() {
        wait_slot(start + WINDOW);
    }
    Ok((sends, start.elapsed().as_nanos() as u64, start))
}

fn wait_slot(at: Instant) {
    const SPIN: Duration = Duration::from_micros(70);
    if let Some(sleep) = at
        .saturating_duration_since(Instant::now())
        .checked_sub(SPIN)
    {
        std::thread::sleep(sleep);
    }
    while Instant::now() < at {
        std::hint::spin_loop();
    }
}

#[derive(Default)]
struct Analysis {
    qualified: bool,
    metrics_valid: bool,
    trace: Option<DutTrace>,
    capture_hz: Option<i32>,
    packets: Vec<capture::Packet>,
    probes: Vec<ProbeResult>,
    sent: u64,
    legal: u64,
    illegal: u64,
    matched: Option<u64>,
    hits: Option<u64>,
    off_target_hits: Option<u64>,
    fault: Option<u64>,
    drop_counts: DropCounts,
    device_ticks: Option<u64>,
    sender_limited: bool,
    offered_pps: Option<f64>,
    data_ok: Option<bool>,
    tx: Option<TxResult>,
}

#[derive(Default)]
struct DropCounts {
    ingress: Option<u64>,
    normal: Option<u64>,
    fault: Option<u64>,
    policy_returned: Option<u64>,
}

impl DropCounts {
    fn from_metrics(pair: &metrics::Pair<'_>, hal: bool) -> Self {
        let (ingress, normal, fault) = if hal {
            (
                "tagged_socket_rx",
                "tagged_socket_discarded",
                "tagged_socket_fault",
            )
        } else {
            (
                "tagged_driver_rx",
                "tagged_driver_normal_drop",
                "tagged_driver_fault",
            )
        };
        Self {
            ingress: pair.delta(ingress),
            normal: pair.delta(normal),
            fault: pair.delta(fault),
            policy_returned: if hal {
                None
            } else {
                pair.delta("tagged_xdp_policy_returned")
            },
        }
    }

    fn policy_enter(&self) -> Option<u64> {
        self.policy_returned
            .filter(|&returned| Some(returned) == self.ingress && self.fault == Some(0))
    }

    fn require(&self, sent: u64) -> Result<()> {
        let ((ingress, normal), fault) = self
            .ingress
            .zip(self.normal)
            .zip(self.fault)
            .context("DROP export lacks tagged ingress, completion, or fault counters")?;
        ensure!(ingress > 0, "no tagged probe reached the DROP boundary");
        ensure!(
            ingress <= sent,
            "tagged DROP ingress exceeds submitted probes"
        );
        ensure!(fault == 0, "a tagged DROP probe had a runtime fault");
        ensure!(
            normal == ingress,
            "a tagged DROP probe lacks normal completion"
        );
        if let Some(returned) = self.policy_returned {
            ensure!(
                returned == ingress,
                "a tagged DROP probe lacks a policy return"
            );
        }
        Ok(())
    }
}

#[derive(Default, Serialize)]
struct TxResult {
    returned: u64,
    duplicate: u64,
    late: u64,
    damaged: u64,
    valid_rtt: u64,
    median_ns: Option<u64>,
    p95_ns: Option<u64>,
    capture_qualified: bool,
}

#[derive(Serialize)]
struct ProbeResult {
    seq: i64,
    returned: bool,
    duplicate: i64,
    late: i64,
    tx_index: Option<i64>,
    rx_index: Option<i64>,
    // Domain crosses PostgreSQL as decimal text to retain a full u64.
    rtt: Option<String>,
    qualified: bool,
}

impl Analysis {
    fn summary(&self, out: &Observation) -> serde_json::Value {
        let ratio = |n: u64, d: u64| (d > 0).then_some(n as f64 / d as f64);
        let dut_rate = |count: Option<u64>| {
            let (count, ticks) = count.zip(self.device_ticks)?;
            ratio(count, ticks).map(|per_tick| per_tick * 1_000_000.0)
        };
        let rx_delta = out
            .rx_before
            .zip(out.rx_after)
            .and_then(|(a, b)| b.checked_sub(a));
        json!({
            "submitted": self.sent, "host_send_ns": out.elapsed,
            "offered_pps": self.offered_pps, "sender_limited": self.sender_limited,
            "sent_legal": self.legal, "sent_illegal": self.illegal,
            "matched_legal": self.matched, "blocked_hit_delta": self.hits,
            "off_target_blacklist_hits": self.off_target_hits,
            "legal_delivery_ratio": self.matched.and_then(|n| ratio(n, self.legal)),
            "illegal_hit_ratio": self.hits.and_then(|n| ratio(n, self.illegal)),
            "token_refused": out.refused, "rxcnt_delta": rx_delta,
            "aggregate_runtime_faults": self.fault, "dut_window_ticks": self.device_ticks,
            "tagged_ingress": self.drop_counts.ingress, "normal_drop": self.drop_counts.normal,
            "tagged_faults": self.drop_counts.fault, "policy_enter": self.drop_counts.policy_enter(),
            "tagged_ingress_pps": dut_rate(self.drop_counts.ingress),
            "completed_drop_pps": dut_rate(self.drop_counts.normal),
            "tx": self.tx,
            "observed_loss": self.tx.as_ref().and_then(|tx| ratio(self.sent - tx.returned, self.sent)),
            "qualified_loss": self.tx.as_ref().filter(|tx| tx.capture_qualified)
                .and_then(|tx| ratio(self.sent - tx.returned, self.sent)),
            "trace_events": self.trace.as_ref().map(|trace| trace.events.len()),
            "outcome_proved": self.data_ok,
        })
    }
}

fn analyze(
    cell: &Cell<'_>,
    link: &Link,
    rules: &[Ipv4Addr],
    run: Uuid,
    stimulus: Stimulus,
    profile: &[u8; 32],
    out: &mut Observation,
) -> Analysis {
    let sent = out.sends.iter().filter(|send| send.submitted).count() as u64;
    let legal = out
        .sends
        .iter()
        .filter(|send| send.submitted && send.class == "legal")
        .count() as u64;
    let illegal = out
        .sends
        .iter()
        .filter(|send| send.submitted && send.class == "illegal")
        .count() as u64;
    let sender_limited = stimulus
        .rate()
        .is_some_and(|rate| sent < rate as u64 * WINDOW.as_secs());
    let offered_pps = stimulus
        .rate()
        .and(out.elapsed)
        .filter(|&ns| ns > 0)
        .map(|ns| sent as f64 * 1e9 / ns as f64);
    let mut result = Analysis {
        sent,
        legal,
        illegal,
        sender_limited,
        offered_pps,
        ..Analysis::default()
    };
    if sent == 0 {
        out.issues.push("no stimulus was submitted".into());
    }
    match cell.experiment {
        Workload::Firewall => analyze_firewall(link, rules, run, stimulus, out, &mut result),
        Workload::Tx => analyze_tx(cell, link, run, out, &mut result),
        Workload::Drop => {}
    }
    let capture = out
        .perf
        .as_deref()
        .zip(out.arm)
        .zip(out.epoch.zip(out.boot))
        .zip(out.freeze);
    if let Some((((bytes, arm), (epoch, boot)), freeze)) = capture {
        result.device_ticks = freeze.tick.checked_sub(arm);
        if cell.variant == "metrics" {
            let validation = metrics::decode(bytes).and_then(|pair| {
                pair.require(run, epoch, boot, profile, arm)?;
                ensure!(
                    pair.final_.tick == freeze.tick && freeze.flags == 0,
                    "NMS1 disagrees with the freeze reply"
                );
                ensure!(
                    pair.baseline.hz == 1_000_000 && pair.baseline.bits == 64 && freeze.tick > arm,
                    "unexpected DUT Metrics clock or window"
                );
                Ok(pair)
            });
            match validation {
                Ok(pair) => {
                    result.metrics_valid = true;
                    result.fault = pair.delta("xdp_runtime_fault");
                    if result.fault.is_some_and(|n| n > 0) {
                        out.issues
                            .push("XDP runtime faults occurred during the window".into());
                    }
                    if !cell.hal()
                        && pair
                            .delta("tagged_driver_fault")
                            .is_some_and(|count| count > 0)
                    {
                        out.issues.push("tagged packet failed in XDP".into());
                    }
                    let ingress_name = if cell.hal() {
                        "tagged_socket_rx"
                    } else {
                        "tagged_driver_rx"
                    };
                    if cell.experiment == Workload::Tx
                        && result.tx.as_ref().is_some_and(|tx| tx.returned == 0)
                        && pair.delta(ingress_name).is_none_or(|count| count == 0)
                    {
                        out.issues
                            .push("no tagged DUT ingress or qualified reply".into());
                    }
                    if cell.experiment == Workload::Drop {
                        result.drop_counts = DropCounts::from_metrics(&pair, cell.hal());
                        if let Err(error) = result.drop_counts.require(sent) {
                            out.issues.push(format!("{error:#}"));
                        }
                    }
                }
                Err(error) => out.issues.push(format!("invalid Metrics: {error:#}")),
            }
        } else {
            match DutTrace::decode(bytes, run, epoch, boot, arm, freeze, cell.hal()) {
                Ok(trace) => {
                    if !trace.complete() {
                        out.issues.push("DUT Trace is empty or overflowed".into());
                    }
                    if !trace.proves_path(cell.experiment, stimulus, cell.hal()) {
                        out.issues
                            .push("Trace lacks the expected tagged packet outcome".into());
                        result.data_ok = Some(false);
                    } else if cell.experiment == Workload::Drop {
                        result.data_ok = Some(true);
                    }
                    result.trace = Some(trace);
                }
                Err(error) => out.issues.push(format!("invalid Trace: {error:#}")),
            }
        }
    } else {
        out.issues
            .push("no validated firmware export or timing boundaries".into());
    }

    let variant_ok = if cell.variant == "metrics" {
        result.metrics_valid
    } else {
        result.trace.as_ref().is_some_and(DutTrace::complete) && result.data_ok == Some(true)
    };
    result.qualified = sent > 0 && variant_ok && out.issues.is_empty();
    result
}

fn analyze_firewall(
    link: &Link,
    rules: &[Ipv4Addr],
    run: Uuid,
    stimulus: Stimulus,
    out: &mut Observation,
    result: &mut Analysis,
) {
    if let Some((before, after)) = out.maps_before.as_ref().zip(out.maps_after.as_ref()) {
        match blacklist_deltas(before, after, rules, link.hit) {
            Some((hits, off_target)) => {
                result.hits = Some(hits);
                result.off_target_hits = Some(off_target);
            }
            None => out
                .issues
                .push("firewall fixture keys changed or a counter decreased".into()),
        }
    }
    if let Some(tokens) = &out.tokens {
        let observed: BTreeSet<_> = tokens.iter().copied().collect();
        result.matched = Some(
            out.sends
                .iter()
                .filter(|send| {
                    send.submitted
                        && send.class == "legal"
                        && observed.contains(&token(run, send.seq))
                })
                .count() as u64,
        );
        if out.sends.iter().any(|send| {
            send.submitted && send.class == "illegal" && observed.contains(&token(run, send.seq))
        }) {
            out.issues
                .push("firewall delivered an illegal token".into());
        }
        if observed.len() as u64 > result.matched.unwrap_or(0) {
            out.issues
                .push("firewall observed tokens outside the legal stimulus".into());
        }
    }
    if result.hits.is_none() || result.matched.is_none() || out.refused.is_none() {
        out.issues
            .push("firewall token or blacklist evidence is incomplete".into());
    }
    if result.hits.is_some_and(|hits| hits > result.illegal) {
        out.issues
            .push("blacklist hit delta exceeds submitted illegal probes".into());
    }
    if out.refused.is_some_and(|n| n > 0) {
        out.issues
            .push("firewall token buffer refused entries".into());
    }
    if matches!(stimulus, Stimulus::Rate(_)) && !has_firewall_observation(result) {
        out.issues
            .push("no DUT observation for either submitted firewall traffic class".into());
    }
    if !matches!(stimulus, Stimulus::Rate(_)) {
        let proved = match stimulus {
            Stimulus::Hit => {
                result.hits == Some(1) && result.matched == Some(0) && result.illegal == 1
            }
            Stimulus::Miss => {
                result.hits == Some(0) && result.matched == Some(1) && result.legal == 1
            }
            _ => false,
        };
        result.data_ok = Some(proved);
        if !proved {
            out.issues
                .push("firewall probe outcome differs from the fixture".into());
        }
    }
}

fn blacklist_deltas(
    before: &Counters,
    after: &Counters,
    rules: &[Ipv4Addr],
    hit: Ipv4Addr,
) -> Option<(u64, u64)> {
    if !before.keys().copied().eq(rules.iter().copied()) || !before.keys().eq(after.keys()) {
        return None;
    }
    let target = after.get(&hit)?.checked_sub(*before.get(&hit)?)?;
    let off_target = before
        .iter()
        .filter(|(ip, _)| **ip != hit)
        .try_fold(0_u64, |sum, (ip, old)| {
            sum.checked_add(after[ip].checked_sub(*old)?)
        })?;
    Some((target, off_target))
}

fn has_firewall_observation(result: &Analysis) -> bool {
    result.matched.is_some_and(|matched| matched > 0) || result.hits.is_some_and(|hits| hits > 0)
}

fn analyze_tx(
    cell: &Cell<'_>,
    link: &Link,
    run: Uuid,
    out: &mut Observation,
    result: &mut Analysis,
) {
    let Some((evidence, cutoff)) = out.capture.as_ref().zip(out.drain_wall) else {
        out.issues
            .push("no just_tx capture or drain boundary".into());
        return;
    };
    let (hz, packets) = match evidence.packets(link, run, cell.hal(), &out.sends) {
        Ok(parsed) => parsed,
        Err(error) => {
            out.issues.push(format!("invalid just_tx pcap: {error:#}"));
            return;
        }
    };
    result.capture_hz = Some(hz);
    let mut by_seq = BTreeMap::<i64, Vec<&capture::Packet>>::new();
    for packet in &packets {
        by_seq.entry(packet.seq).or_default().push(packet);
    }
    let mut tx = TxResult {
        capture_qualified: evidence.complete() && result.sent > 0,
        ..TxResult::default()
    };
    let mut rtts = Vec::new();
    for send in out.sends.iter().filter(|send| send.submitted) {
        let records = by_seq.get(&send.seq).map(Vec::as_slice).unwrap_or_default();
        let outgoing: Vec<_> = records
            .iter()
            .copied()
            .filter(|p| p.tx && p.valid())
            .collect();
        let sent = outgoing.first().copied();
        let mut receives: Vec<_> = records
            .iter()
            .copied()
            .filter(|p| !p.tx && p.valid())
            .collect();
        receives.sort_by_key(|p| p.ns(hz));
        // The same host realtime clock stamps pcap records and the drain deadline.
        let split = receives.partition_point(|p| p.ns(hz) <= cutoff);
        let got = receives[..split].first().copied();
        let duplicates = split.saturating_sub(1) as i64;
        let late = (receives.len() - split) as i64;
        let damage = records.iter().filter(|p| !p.tx && !p.valid()).count() as u64;
        tx.returned += u64::from(got.is_some());
        tx.duplicate += duplicates as u64;
        tx.late += late as u64;
        tx.damaged += damage;
        // Verify the capture clock against the corresponding local send record.
        // Allow one clock quantum of timestamp rounding.
        let clock_ok = sent.is_some_and(|p| {
            p.ns(hz).saturating_add(1_000_000_000 / hz as u64) >= send.wall && p.ns(hz) <= cutoff
        });
        let rtt = sent
            .zip(got)
            .and_then(|(t, r)| r.ns(hz).checked_sub(t.ns(hz)));
        let qualified = evidence.complete()
            && outgoing.len() == 1
            && clock_ok
            && damage == 0
            && (got.is_none() || rtt.is_some());
        tx.capture_qualified &= qualified;
        if let Some(rtt) = rtt.filter(|_| qualified) {
            rtts.push(rtt);
        }
        result.probes.push(ProbeResult {
            seq: send.seq,
            returned: got.is_some(),
            duplicate: duplicates,
            late,
            tx_index: sent.map(|p| p.index),
            rx_index: got.map(|p| p.index),
            rtt: rtt.filter(|_| qualified).map(|ns| ns.to_string()),
            qualified,
        });
    }
    rtts.sort_unstable();
    tx.valid_rtt = rtts.len() as u64;
    if !rtts.is_empty() {
        let middle = rtts.len() / 2;
        tx.median_ns = Some(rtts[(rtts.len() - 1) / 2].midpoint(rtts[middle]));
        tx.p95_ns = Some(rtts[(95 * rtts.len()).div_ceil(100) - 1]);
    }
    if !tx.capture_qualified {
        out.issues.push(
            "capture loss, damage, missing TX evidence, or inconsistent host timestamps".into(),
        );
    }
    let rx = out
        .rx_before
        .zip(out.rx_after)
        .and_then(|(b, a)| a.checked_sub(b));
    if !rx.is_some_and(|delta| delta >= tx.returned && delta <= result.sent) {
        out.issues
            .push("rxcnt is absent, decreased, or disagrees with the stimulus and capture".into());
    }
    if cell.variant == "trace" {
        let proved = tx.capture_qualified && result.sent == 1 && tx.returned == 1 && rx == Some(1);
        result.data_ok = Some(proved);
        if !proved {
            out.issues
                .push("just_tx probe did not return a qualified JTX1 reply".into());
        }
    }
    result.packets = packets;
    result.tx = Some(tx);
}

// Network Trace dictionary 3. The ESP32-C3 supplies all ticks in microseconds.
const TRACE_DICTIONARY: i32 = 3;
const EVENT_NAMES: [&str; 28] = [
    "driver_rx",
    "xdp_run_enter",
    "xdp_action_pass",
    "xdp_action_drop",
    "xdp_action_tx",
    "xdp_action_aborted",
    "driver_rx_enqueued",
    "driver_rx_discarded",
    "driver_rx_overflow",
    "driver_tx_submitted",
    "driver_tx_submit_failed",
    "driver_tx_completed",
    "driver_tx_completion_failed",
    "xdp_runtime_fault",
    "socket_rx",
    "socket_discarded",
    "socket_delivered",
    "socket_send_queued",
    "socket_send_failed",
    "socket_address_rejected",
    "socket_send_begin",
    "driver_tx_enqueue_failed",
    "driver_rx_end",
    "driver_tx_submit_begin",
    "driver_normal_drop",
    "driver_packet_fault",
    "driver_tx_enqueued",
    "xdp_policy_returned",
];

struct DutTrace {
    epoch: u32,
    boot: u64,
    capacity: i32,
    dropped: i64,
    flags: i64,
    events: Vec<DutEvent>,
}

#[derive(Serialize)]
struct DutEvent {
    sequence: i64,
    id: i32,
    context: i32,
    tick: String,
    probe_seq: Option<i64>,
    arg0: i64,
    arg1: i64,
}

impl DutTrace {
    fn complete(&self) -> bool {
        self.flags == 0 && self.dropped == 0 && !self.events.is_empty()
    }

    fn proves_path(&self, experiment: Workload, stimulus: Stimulus, hal: bool) -> bool {
        let expected: &[i32] = match (hal, experiment, stimulus) {
            (true, Workload::Firewall, Stimulus::Miss) => &[15, 17],
            (true, Workload::Firewall | Workload::Drop, _) => &[15, 16],
            (true, Workload::Tx, _) => &[15, 21, 18],
            (false, Workload::Firewall, Stimulus::Miss) => &[1, 2, 3, 28, 7, 23],
            (false, Workload::Firewall | Workload::Drop, _) => &[1, 2, 4, 28, 8, 25, 23],
            (false, Workload::Tx, _) => &[1, 2, 5, 28, 27, 23],
        };
        self.complete()
            && self
                .events
                .iter()
                .map(|event| (event.id, event.probe_seq))
                .eq(expected.iter().map(|&id| (id, Some(1))))
    }

    fn decode(
        bytes: &[u8],
        run: Uuid,
        epoch: u32,
        boot: u64,
        arm: u64,
        freeze: PerfFreeze,
        hal: bool,
    ) -> Result<Self> {
        ensure!(bytes.len() >= 57, "short Trace header");
        let u16_at = |at| u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap());
        let u32_at = |at| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let u64_at = |at| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        ensure!(
            u16_at(0) == 1 && i32::from(u16_at(2)) == TRACE_DICTIONARY,
            "unknown network Trace version"
        );
        ensure!(
            bytes[4..20] == *run.as_bytes() && u32_at(20) == epoch && u64_at(24) == boot,
            "Trace disagrees with prepared identity"
        );
        ensure!(
            u64_at(32) == 1_000_000 && bytes[40] == 64,
            "unexpected Trace clock"
        );
        let capacity = i32::try_from(u32_at(41))?;
        let written = u32_at(45) as usize;
        let dropped = i64::from(u32_at(49));
        let flags = i64::from(u32_at(53));
        ensure!(
            capacity > 0
                && written <= capacity as usize
                && (bytes.len() - 57) / 40 == written
                && (bytes.len() - 57) % 40 == 0,
            "invalid Trace extent"
        );
        ensure!(
            freeze.tick >= arm
                && flags == i64::from(freeze.flags)
                && flags == i64::from(dropped > 0),
            "Trace flags or freeze boundary disagree"
        );
        let mut events = Vec::with_capacity(written);
        let mut prior = arm;
        for i in 0..written {
            let at = 57 + 40 * i;
            let id = i32::from(u16_at(at + 4));
            let context = i32::from(bytes[at + 7]);
            let tick = u64_at(at + 8);
            let probe = u64_at(at + 16);
            let socket = (15..=21).contains(&id);
            ensure!(
                (1..=EVENT_NAMES.len() as i32).contains(&id)
                    && socket == hal
                    && context == if hal { 2 } else { 1 },
                "unknown Trace event"
            );
            ensure!(
                bytes[at + 6] == 3 && u32_at(at + 24) == 0 && u32_at(at + 28) == 0,
                "network Trace requires instant observations"
            );
            ensure!(
                u32_at(at) as usize == i && (prior..=freeze.tick).contains(&tick),
                "Trace order"
            );
            prior = tick;
            ensure!(probe == 1, "Trace event names an unexpected probe");
            ensure!(
                hal || !matches!(id, 10..=14 | 24),
                "asynchronous or aggregate event carries a packet tag"
            );
            let probe_seq = Some(i64::try_from(probe)?);
            events.push(DutEvent {
                sequence: i as i64,
                id,
                context,
                tick: tick.to_string(),
                probe_seq,
                arg0: i64::from(u32_at(at + 32)),
                arg1: i64::from(u32_at(at + 36)),
            });
        }
        Ok(Self {
            epoch,
            boot,
            capacity,
            dropped,
            flags,
            events,
        })
    }
}

#[cfg(test)]
mod attribution_tests {
    use super::*;

    #[test]
    fn hal_drop_switches_policy_to_acknowledge_readiness() {
        assert!(Workload::Drop.toggles_policy("hal"));
        assert!(!Workload::Firewall.toggles_policy("hal"));
        assert!(!Workload::Tx.toggles_policy("hal"));
    }

    #[test]
    fn firewall_keeps_other_key_hits_separate_from_the_stimulus() {
        let hit = Ipv4Addr::new(192, 0, 2, 10);
        let other = Ipv4Addr::new(192, 0, 2, 20);
        let rules = [hit, other];
        let before = [(hit, 0), (other, 1)].into();
        let mut after: Counters = [(hit, 1800), (other, 3)].into();
        assert_eq!(
            blacklist_deltas(&before, &after, &rules, hit),
            Some((1800, 2))
        );
        after.insert(other, 0);
        assert_eq!(blacklist_deltas(&before, &after, &rules, hit), None);
        after.remove(&other);
        assert_eq!(blacklist_deltas(&before, &after, &rules, hit), None);
    }

    #[test]
    fn firewall_window_needs_dut_evidence_from_a_traffic_class() {
        let mut result = Analysis {
            sent: 2000,
            legal: 200,
            illegal: 1800,
            matched: Some(0),
            hits: Some(0),
            ..Analysis::default()
        };
        assert!(!has_firewall_observation(&result));
        result.hits = Some(5);
        assert!(has_firewall_observation(&result));
        result.hits = Some(0);
        result.matched = Some(1);
        assert!(has_firewall_observation(&result));
    }

    #[test]
    fn drop_windows_need_positive_tagged_ingress_and_normal_completion() {
        let mut sample = DropCounts {
            ingress: Some(90),
            normal: Some(90),
            fault: Some(0),
            policy_returned: Some(90),
        };
        assert!(sample.require(100).is_ok());
        assert_eq!(sample.policy_enter(), Some(90));
        assert!(sample.require(89).is_err());
        sample.normal = Some(89);
        assert!(sample.require(100).is_err());
        sample.fault = Some(1);
        assert!(sample.require(100).is_err());
        assert_eq!(sample.policy_enter(), None);
        sample.normal = None;
        assert!(sample.require(100).is_err());
    }

    fn trace(ids: &[i32]) -> DutTrace {
        DutTrace {
            epoch: 1,
            boot: 1,
            capacity: 64,
            dropped: 0,
            flags: 0,
            events: ids
                .iter()
                .enumerate()
                .map(|(sequence, &id)| DutEvent {
                    sequence: sequence as i64,
                    id,
                    context: 1,
                    tick: sequence.to_string(),
                    probe_seq: Some(1),
                    arg0: 0,
                    arg1: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn tagged_driver_trace_requires_policy_provenance_and_the_sync_outcome() {
        assert!(trace(&[1, 2, 4, 28, 8, 25, 23]).proves_path(
            Workload::Drop,
            Stimulus::Tagged,
            false
        ));
        assert!(!trace(&[1, 2, 4, 8, 23]).proves_path(Workload::Drop, Stimulus::Tagged, false));
        assert!(!trace(&[1, 2, 6, 26, 8, 23]).proves_path(Workload::Drop, Stimulus::Tagged, false));
        assert!(trace(&[1, 2, 3, 28, 7, 23]).proves_path(
            Workload::Firewall,
            Stimulus::Miss,
            false
        ));
        assert!(trace(&[1, 2, 5, 28, 27, 23]).proves_path(Workload::Tx, Stimulus::Tagged, false));
        assert!(!trace(&[1, 2, 5, 28, 12, 23]).proves_path(Workload::Tx, Stimulus::Tagged, false));
    }
}
