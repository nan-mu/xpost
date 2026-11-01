//! One firmware, two cold images, tcpdump text in Postgres.
//!
//! `case_swap` swaps the IPv4 addresses and UDP ports, then returns `XDP_TX`.
//! `case_mark` writes destination `203.0.113.77` and returns `XDP_TX`.
//! Both increment `rxcnt[17]` and leave the UDP payload unchanged.
//!
//! Map access stays outside the leased window. Warmup watches the wire, then
//! the exam disables XDP, drains, clears `rxcnt`, and reads the baseline.
//! It enables only to send `CASE0`..`CASE3`, disables, drains, and reads the
//! result. Those payloads are five bytes, so a firmware `ACK\n` cannot satisfy
//! the check. The firmware ELF is flashed once. tcpdump writes no pcap.

use std::future::Future;
use std::net::Ipv4Addr;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::agent::AgentClient;
use crate::config::{FileConfig, format_mac};
use crate::db::{self, CaseObs, PgOpts};
use crate::pipeline::{self, BudgetEta, READY_BUDGET, RoundUi, status};

const FW: &str = "case-cag";
pub(crate) const PORT: u16 = 8080;
pub(crate) const SENT: usize = 4;
const IPPROTO_UDP: u32 = 17;
pub(crate) const STIM_SRC: Ipv4Addr = Ipv4Addr::new(192, 168, 110, 50);
pub(crate) const MARK_DST: &str = "203.0.113.77";
const READY_TRIES: u32 = 40;
const DUMP_CAP: usize = 64 * 1024;
const DRAIN: Duration = Duration::from_secs(1);
pub(crate) const PAYLOAD_LEN: usize = 5;

struct Step {
    seq: i32,
    xdp: &'static str,
    map: &'static str,
    slot: u32,
    action: &'static str,
    mark: bool,
    capability: &'static str,
    expect: &'static str,
    replace: bool,
}

const STEPS: &[Step] = &[
    Step {
        seq: 1,
        xdp: "case_swap",
        map: "rxcnt",
        slot: IPPROTO_UDP,
        action: "XDP_TX",
        mark: false,
        capability: "count, rewrite",
        expect: "While XDP is disabled, rxcnt[17] is cleared and read. The scored window then sends CASE0..CASE3 and returns XDP_TX with Ethernet MACs, IPv4 addresses, and UDP ports swapped. Each token appears in a DUT-sourced frame of UDP length 5. rxcnt[17] increases by at least 4; a larger delta is not one-count-per-packet. Firmware ACK\\n is UDP length 4 and does not carry these tokens.",
        replace: false,
    },
    Step {
        seq: 2,
        xdp: "case_mark",
        map: "rxcnt",
        slot: IPPROTO_UDP,
        action: "XDP_TX",
        mark: true,
        capability: "count, rewrite",
        expect: "While XDP is disabled, rxcnt[17] is cleared and read. The scored window then sends CASE0..CASE3 and returns XDP_TX with Ethernet MACs swapped and IPv4 destination 203.0.113.77. Each token appears in a DUT-sourced frame of UDP length 5. rxcnt[17] increases by at least 4; a larger delta is not one-count-per-packet. Router forwards of that destination are not DUT replies.",
        replace: true,
    },
];

pub async fn run_case(cfg: &FileConfig) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("case needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint);
    let pg_opts = PgOpts { formal: true };
    let client = db::connect_and_ensure(pg_opts).await?;
    let exam_id = db::insert_exam(&client, Some("case")).await?;
    eprintln!("case exam={exam_id} firmware={FW}");

    let ui = RoundUi::new();
    let mut eta = BudgetEta::new();
    eta.add_flashes(1);
    ui.begin_round();
    ui.check_pending(&format!("烧录 `{FW}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.check_pending("DUT ready");
    status::flash_idle(&ui, &agent, FW, Duration::from_secs(300), &mut eta).await?;
    status::settle_checklist(&ui, &agent, READY_BUDGET).await?;

    for step in STEPS {
        run_step(cfg, &agent, &client, exam_id, step).await?;
    }
    db::stop_exam(&client, exam_id).await?;
    eprintln!("case ok exam={exam_id}");
    Ok(())
}

async fn run_step(
    cfg: &FileConfig,
    agent: &AgentClient,
    client: &tokio_postgres::Client,
    exam_id: db::ExamId,
    step: &Step,
) -> Result<()> {
    let mut events = Vec::new();
    events.push(format!("fw={FW} xdp={} host_ms={}", step.xdp, host_ms()));
    if step.replace {
        let mac = format_mac(&cfg.esp_mac_bytes()?);
        let old_needle = format!(
            "{}.{} > {}.{}",
            cfg.target.esp_ip, PORT, STIM_SRC, cfg.target.stim_sport
        );
        agent
            .enable_xdp()
            .await
            .with_context(|| format!("lease previous image before {}", step.xdp))?;
        let gap = capture_update(
            cfg,
            &mac,
            &old_needle,
            MARK_DST,
            CaptureWindow {
                interval: Duration::from_millis(2),
                preamble: Duration::from_millis(400),
                tail: Duration::from_secs(30),
            },
            || async {
                let t0 = unix_secs();
                agent
                    .disable_xdp()
                    .await
                    .with_context(|| format!("disable xdp before {}", step.xdp))?;
                agent
                    .clear_xdp("*")
                    .await
                    .with_context(|| format!("clear xdp before {}", step.xdp))?;
                agent
                    .flash_xdp(step.xdp)
                    .await
                    .with_context(|| format!("flash xdp {}", step.xdp))?;
                agent
                    .get_init()
                    .await
                    .with_context(|| format!("OK init after {}", step.xdp))?;
                Ok(t0)
            },
        )
        .await?;
        events.push(format!(
            "cold_command_ms={} old_before={} old_after={} new_replies={}",
            gap.command_ms
                .map(|ms| format!("{ms:.3}"))
                .unwrap_or_else(|| "none".to_string()),
            gap.old_before,
            gap.old_after,
            gap.new_replies
        ));
        eprintln!("case cold update {}", gap.summary);
        if gap.command_ms.is_none() {
            bail!("case cold update has no frame at or after disable:\n{}", gap.report);
        }
    } else {
        agent
            .clear_xdp("*")
            .await
            .with_context(|| format!("clear xdp before {}", step.xdp))?;
        events.push(format!("clear_cold {}", host_ms()));
        agent
            .flash_xdp(step.xdp)
            .await
            .with_context(|| format!("flash xdp {}", step.xdp))?;
        events.push(format!("install {} {}", step.xdp, host_ms()));
        agent
            .get_init()
            .await
            .with_context(|| format!("OK init after {}", step.xdp))?;
        events.push(format!("init {}", host_ms()));
    }

    // Boot leases the image. Stay out of rxcnt until that lease is released.
    agent
        .enable_xdp()
        .await
        .with_context(|| format!("enable xdp {}", step.xdp))?;
    events.push(format!("enable_warmup {}", host_ms()));
    let mac = format_mac(&cfg.esp_mac_bytes()?);
    warmup(cfg, step, &mac).await?;

    agent
        .disable_xdp()
        .await
        .with_context(|| format!("disable xdp before baseline {}", step.xdp))?;
    events.push(format!("disable_baseline {}", host_ms()));
    tokio::time::sleep(DRAIN).await;
    agent
        .clear_map(step.map)
        .await
        .with_context(|| format!("clear map {}", step.map))?;
    let before = agent.map_slot(step.map, step.slot).await?;
    events.push(format!("map_before {before} {}", host_ms()));
    if before != 0 {
        bail!(
            "case {} baseline rxcnt[{}]={before} after disable and clear",
            step.xdp,
            step.slot
        );
    }

    agent
        .enable_xdp()
        .await
        .with_context(|| format!("enable xdp for send {}", step.xdp))?;
    events.push(format!("enable_send {}", host_ms()));
    let scored: Vec<[u8; PAYLOAD_LEN]> = (0..SENT).map(payload).collect();
    let refs: Vec<&[u8]> = scored.iter().map(|p| p.as_slice()).collect();
    let dump = sniff(cfg, &mac, &refs, Duration::from_millis(800)).await?;
    agent
        .disable_xdp()
        .await
        .with_context(|| format!("disable xdp before read {}", step.xdp))?;
    events.push(format!("disable_read {}", host_ms()));
    tokio::time::sleep(DRAIN).await;
    let after = agent.map_slot(step.map, step.slot).await?;
    events.push(format!("map_after {after} {}", host_ms()));

    let needle = if step.mark {
        MARK_DST.to_string()
    } else {
        format!(
            "{}.{} > {}.{}",
            cfg.target.esp_ip, PORT, STIM_SRC, cfg.target.stim_sport
        )
    };
    let hits = payload_hits(&dump, &mac, &needle);
    let matched = hits.iter().filter(|hit| **hit).count();
    let saw_reply = matched == SENT;
    let delta = after.saturating_sub(before);
    // Retries can increment rxcnt more than once per send. Do not require equality.
    let ok = before == 0 && delta >= SENT as u64 && saw_reply;
    eprintln!(
        "case seq={} xdp={} ok={ok} action={} {}[{}] {before}->{after} sent={SENT} needle={needle} payloads={matched}/{SENT}",
        step.seq, step.xdp, step.action, step.map, step.slot
    );
    let stored = store_text(&events, &hits, &dump);
    let excerpt: String = stored.lines().take(16).collect::<Vec<_>>().join("\n");
    eprintln!("case tcpdump excerpt:\n{excerpt}");

    db::insert_case_obs(
        client,
        exam_id,
        &CaseObs {
            seq: step.seq,
            xdp: step.xdp,
            capability: step.capability,
            action: step.action,
            expect: step.expect,
            map_name: step.map,
            map_slot: step.slot as i32,
            map_before: before as i64,
            map_after: after as i64,
            sent: SENT as i32,
            saw_reply,
            ok,
            tcpdump: &stored,
        },
    )
    .await?;
    if !ok {
        bail!(
            "case {} mismatch: map {before}->{after} (want at least +{SENT} after a zero baseline), payloads {matched}/{SENT} needle={needle}",
            step.xdp
        );
    }
    Ok(())
}

async fn warmup(cfg: &FileConfig, step: &Step, mac: &str) -> Result<()> {
    let warm = b"WARM!";
    for i in 0..READY_TRIES {
        let dump = sniff(cfg, mac, &[warm], Duration::from_millis(400)).await?;
        if dump.to_ascii_lowercase().contains(&format!("{mac} >")) {
            eprintln!("case {} link up", step.xdp);
            return Ok(());
        }
        if i % 5 == 4 {
            eprintln!("case {} still waiting for a DUT frame", step.xdp);
        }
    }
    bail!("case {} saw no DUT frame during warmup", step.xdp)
}

pub(crate) async fn sniff(
    cfg: &FileConfig,
    mac: &str,
    payloads: &[&[u8]],
    after: Duration,
) -> Result<String> {
    let mut child = Command::new("tcpdump")
        .args([
            "-e",
            "-n",
            "-l",
            "-tt",
            "-X",
            "-i",
            &cfg.probe.iface,
            "udp",
            "and",
            "port",
            "8080",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn tcpdump")?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_payloads(cfg, payloads).await?;
    tokio::time::sleep(after).await;
    child.start_kill().ok();
    let mut stdout = child.stdout.take().context("tcpdump stdout")?;
    let mut stderr = child.stderr.take().context("tcpdump stderr")?;
    let mut out = String::new();
    let mut err = String::new();
    let _ = child.wait().await;
    stdout.read_to_string(&mut out).await.context("read tcpdump stdout")?;
    stderr.read_to_string(&mut err).await.context("read tcpdump stderr")?;
    let mut text = format!("# dut_mac {mac}\n{out}");
    if !err.is_empty() {
        text.push_str("\n# stderr\n");
        text.push_str(&err);
    }
    Ok(text)
}

/// How long to sample the old behavior, and how long to keep sampling after the update returns.
pub(crate) struct CaptureWindow {
    pub interval: Duration,
    pub preamble: Duration,
    pub tail: Duration,
}

/// Command-to-capture latency on the probe host.
///
/// `command_ms` is the tcpdump `-tt` time of the first new DUT frame at or
/// after `t0`, minus `t0`. Both clocks are this host. The number includes the
/// path from the test process to the DUT and one air hop. `gap_ms` is the
/// last-old to first-new arrival gap and is not the reported latency.
pub(crate) struct WireGap {
    pub command_ms: Option<f64>,
    pub old_before: usize,
    pub old_after: usize,
    pub new_replies: usize,
    pub summary: String,
    pub report: String,
}

/// Run tcpdump across `update` while a UDP stimulus keeps flowing.
///
/// `update` returns the unix-second timestamp taken immediately before the
/// timed command. The result is the first new DUT frame with `ts >= t0`.
pub(crate) async fn capture_update<F, Fut>(
    cfg: &FileConfig,
    mac: &str,
    old_needle: &str,
    new_needle: &str,
    window: CaptureWindow,
    update: F,
) -> Result<WireGap>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<f64>>,
{
    let mut child = Command::new("tcpdump")
        .args([
            "-e",
            "-n",
            "-l",
            "-tt",
            "-X",
            "-i",
            &cfg.probe.iface,
            "udp",
            "and",
            "port",
            "8080",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn tcpdump")?;
    let mut stdout = child.stdout.take().context("tcpdump stdout")?;
    let mut stderr = child.stderr.take().context("tcpdump stderr")?;
    let reader = tokio::spawn(async move {
        let mut out = String::new();
        let mut err = String::new();
        let _ = tokio::join!(stdout.read_to_string(&mut out), stderr.read_to_string(&mut err));
        (out, err)
    });

    let stop = Arc::new(AtomicBool::new(false));
    let sender_stop = Arc::clone(&stop);
    let iface = cfg.probe.iface.clone();
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => pipeline::iface_mac(&iface)?,
    };
    let dst_mac = cfg.esp_mac_bytes()?;
    let esp = cfg.target.esp_ip;
    let sport = cfg.target.stim_sport;
    let interval = window.interval;
    let sender = tokio::task::spawn_blocking(move || {
        let raw = pipeline::RawTx::open(&iface, src_mac, dst_mac, esp, PORT, sport)?;
        raw.announce_neighbor(STIM_SRC)?;
        let mut next_arp = std::time::Instant::now();
        while !sender_stop.load(Ordering::Relaxed) {
            if next_arp.elapsed() >= Duration::from_secs(1) {
                raw.announce_neighbor(STIM_SRC)?;
                next_arp = std::time::Instant::now();
            }
            raw.send_udp(STIM_SRC, esp, PORT, b"CASE0")?;
            std::thread::sleep(interval);
        }
        Ok::<(), anyhow::Error>(())
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::sleep(window.preamble).await;
    let updated = update().await;
    tokio::time::sleep(window.tail).await;
    stop.store(true, Ordering::Relaxed);
    let _ = sender.await;
    child.start_kill().ok();
    let _ = child.wait().await;
    let (out, err) = reader.await.context("tcpdump reader")?;
    let mut text = format!("# dut_mac {mac}\n{out}");
    if !err.is_empty() {
        text.push_str("\n# stderr\n");
        text.push_str(&err);
    }
    let command_at = updated.as_ref().copied().unwrap_or(0.0);
    let gap = wire_gap(&text, mac, old_needle, new_needle, command_at);
    updated.with_context(|| gap.summary.clone())?;
    Ok(gap)
}

fn wire_gap(dump: &str, mac: &str, old_needle: &str, new_needle: &str, command_at: f64) -> WireGap {
    let src = format!("{mac} >");
    let mut old: Vec<(f64, String)> = Vec::new();
    let mut new: Vec<(f64, String)> = Vec::new();
    for pkt in packets(dump) {
        let Some(header) = pkt.lines().next() else {
            continue;
        };
        if !header.to_ascii_lowercase().contains(&src.to_ascii_lowercase()) {
            continue;
        }
        if !header.contains(&format!("UDP, length {PAYLOAD_LEN}")) {
            continue;
        }
        let Some(ts) = frame_ts(header) else {
            continue;
        };
        if header.contains(old_needle) {
            old.push((ts, header.to_string()));
        } else if header.contains(new_needle) {
            new.push((ts, header.to_string()));
        }
    }
    let first_new = new
        .iter()
        .map(|(ts, _)| *ts)
        .filter(|ts| *ts >= command_at)
        .reduce(f64::min);
    let last_old = first_new.and_then(|cut| {
        old.iter()
            .map(|(ts, _)| *ts)
            .filter(|ts| *ts < cut)
            .reduce(f64::max)
    });
    let old_before = old.iter().filter(|(ts, _)| *ts < command_at).count();
    let old_after = old.iter().filter(|(ts, _)| *ts >= command_at).count();
    let gap_ms = match (last_old, first_new) {
        (Some(from), Some(to)) => Some((to - from) * 1_000.0),
        _ => None,
    };
    let command_ms = first_new.map(|ts| (ts - command_at) * 1_000.0);
    let summary = format!(
        "command_ms={} gap_ms={} old_before={} old_after={} new_replies={} t0={} first_new={}",
        command_ms
            .map(|ms| format!("{ms:.3}"))
            .unwrap_or_else(|| "none".to_string()),
        gap_ms
            .map(|ms| format!("{ms:.3}"))
            .unwrap_or_else(|| "none".to_string()),
        old_before,
        old_after,
        new.iter().filter(|(ts, _)| *ts >= command_at).count(),
        format!("{command_at:.6}"),
        first_new
            .map(|ts| format!("{ts:.6}"))
            .unwrap_or_else(|| "none".to_string()),
    );
    let mut report = format!("# {summary}\n# needle_old {old_needle}\n# needle_new {new_needle}\n");
    report.push_str("# boundary old\n");
    let shown_old: Vec<_> = old.iter().filter(|(ts, _)| *ts < command_at).collect();
    let start = shown_old.len().saturating_sub(3);
    for (ts, header) in &shown_old[start..] {
        report.push_str(&format!("{ts:.6} old {header}\n"));
    }
    if old_after > 0 {
        report.push_str("# old after t0\n");
        for (ts, header) in old.iter().filter(|(ts, _)| *ts >= command_at).take(3) {
            report.push_str(&format!("{ts:.6} old {header}\n"));
        }
    }
    if first_new.is_some() {
        report.push_str("# boundary new\n");
        for (ts, header) in new.iter().filter(|(ts, _)| *ts >= command_at).take(3) {
            report.push_str(&format!("{ts:.6} new {header}\n"));
        }
    }
    WireGap {
        command_ms,
        old_before,
        old_after,
        new_replies: new.iter().filter(|(ts, _)| *ts >= command_at).count(),
        summary,
        report,
    }
}

pub(crate) fn unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn frame_ts(header: &str) -> Option<f64> {
    let (stamp, _) = header.split_once(' ')?;
    stamp.parse().ok()
}

async fn send_payloads(cfg: &FileConfig, payloads: &[&[u8]]) -> Result<()> {
    let iface = cfg.probe.iface.clone();
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => pipeline::iface_mac(&iface)?,
    };
    let dst_mac = cfg.esp_mac_bytes()?;
    let esp = cfg.target.esp_ip;
    let sport = cfg.target.stim_sport;
    let owned: Vec<Vec<u8>> = payloads.iter().map(|p| p.to_vec()).collect();
    tokio::task::spawn_blocking(move || {
        let raw = pipeline::RawTx::open(&iface, src_mac, dst_mac, esp, PORT, sport)?;
        raw.announce_neighbor(STIM_SRC)?;
        for payload in &owned {
            raw.send_udp(STIM_SRC, esp, PORT, payload)?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("stimulus thread")?
}

pub(crate) fn payload(i: usize) -> [u8; PAYLOAD_LEN] {
    let mut body = *b"CASE0";
    body[4] = b'0' + u8::try_from(i).unwrap_or(0);
    body
}

/// DUT-sourced frames whose header has the rewrite and whose dump contains `CASEn`.
pub(crate) fn payload_hits(dump: &str, mac: &str, needle: &str) -> [bool; SENT] {
    let src = format!("{mac} >");
    let mut hits = [false; SENT];
    for pkt in packets(dump) {
        let Some(header) = pkt.lines().next() else {
            continue;
        };
        if !header.to_ascii_lowercase().contains(&src.to_ascii_lowercase()) {
            continue;
        }
        if !header.contains(needle) || !header.contains(&format!("UDP, length {PAYLOAD_LEN}")) {
            continue;
        }
        // The fifth payload byte sits on the next hex line. Join only the ASCII columns.
        let ascii = ascii_columns(&pkt);
        for (i, hit) in hits.iter_mut().enumerate() {
            if ascii.contains(&format!("CASE{i}")) {
                *hit = true;
            }
        }
    }
    hits
}

fn ascii_columns(pkt: &str) -> String {
    let mut ascii = String::new();
    for line in pkt.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("0x") {
            continue;
        }
        if let Some((_, tail)) = trimmed.rsplit_once("  ") {
            ascii.push_str(tail.trim());
        }
    }
    ascii
}

fn packets(dump: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in dump.lines() {
        if line_is_frame(line) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if line_is_frame(line) || !cur.is_empty() {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn line_is_frame(line: &str) -> bool {
    let Some((sec, rest)) = line.split_once('.') else {
        return false;
    };
    let Some((frac, tail)) = rest.split_once(' ') else {
        return false;
    };
    !sec.is_empty()
        && sec.bytes().all(|b| b.is_ascii_digit())
        && !frac.is_empty()
        && frac.bytes().all(|b| b.is_ascii_digit())
        && !tail.is_empty()
}

fn store_text(events: &[String], hits: &[bool; SENT], dump: &str) -> String {
    let mut text = String::from(
        "# host control times are not a measured datapath interruption\n",
    );
    for event in events {
        text.push_str("# ");
        text.push_str(event);
        text.push('\n');
    }
    text.push_str("# payloads");
    for (i, hit) in hits.iter().enumerate() {
        text.push_str(&format!(" CASE{i}={}", if *hit { "yes" } else { "no" }));
    }
    text.push('\n');
    text.push_str(dump);
    if text.len() > DUMP_CAP {
        text.truncate(DUMP_CAP);
        text.push_str("\n[truncated]\n");
    }
    text
}

fn host_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
