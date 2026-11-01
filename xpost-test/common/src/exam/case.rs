//! One firmware, two cold images, tcpdump text in Postgres.
//!
//! `case_swap` writes destination `203.0.113.10` and returns `XDP_TX`.
//! `case_mark` writes destination `203.0.113.77` and returns `XDP_TX`.
//! Both increment `rxcnt[17]` and leave the UDP payload unchanged.
//!
//! Map access stays outside the leased window. Warmup watches the wire, then
//! the exam disables XDP, drains, clears `rxcnt`, and reads the baseline.
//! It enables only to send `CASE0`..`CASE3`, disables, drains, and reads the
//! result. Those payloads are five bytes, so a firmware `ACK\n` cannot satisfy
//! the check. The firmware ELF is flashed once. tcpdump writes no pcap.

use std::net::Ipv4Addr;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
pub(crate) const DST_A: &str = "203.0.113.10";
pub(crate) const MARK_DST: &str = "203.0.113.77";
const READY_TRIES: u32 = 40;
const DUMP_CAP: usize = 64 * 1024;
const DRAIN: Duration = Duration::from_secs(1);
const REPEATS: usize = 100;
pub(crate) const PAYLOAD_LEN: usize = 5;

struct Step {
    seq: i32,
    xdp: &'static str,
    map: &'static str,
    slot: u32,
    action: &'static str,
    dst: &'static str,
    capability: &'static str,
    expect: &'static str,
}

const STEPS: &[Step] = &[
    Step {
        seq: 1,
        xdp: "case_swap",
        map: "rxcnt",
        slot: IPPROTO_UDP,
        action: "XDP_TX",
        dst: DST_A,
        capability: "count, rewrite",
        expect: "While XDP is disabled, rxcnt[17] is cleared and read. The scored window then sends CASE0..CASE3 and returns XDP_TX with Ethernet MACs swapped and IPv4 destination 203.0.113.10. Each token appears in a DUT-sourced frame of UDP length 5. rxcnt[17] increases by at least 4; a larger delta is not one-count-per-packet. Router forwards of that destination are not DUT replies.",
    },
    Step {
        seq: 2,
        xdp: "case_mark",
        map: "rxcnt",
        slot: IPPROTO_UDP,
        action: "XDP_TX",
        dst: MARK_DST,
        capability: "count, rewrite",
        expect: "While XDP is disabled, rxcnt[17] is cleared and read. The scored window then sends CASE0..CASE3 and returns XDP_TX with Ethernet MACs swapped and IPv4 destination 203.0.113.77. Each token appears in a DUT-sourced frame of UDP length 5. rxcnt[17] increases by at least 4; a larger delta is not one-count-per-packet. Router forwards of that destination are not DUT replies.",
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
    run_cold_latency(cfg, &agent, &client, exam_id).await?;
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

    let needle = step.dst.to_string();
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

pub(crate) fn unix_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `mcu_us` is the device clock in the reply. `agent_rx` is the agent host
/// clock when that reply was read, appended as `agent_rx=<unix seconds>`.
pub(crate) fn reply_stamps(msg: &str) -> Result<(u32, f64)> {
    let (head, tail) = msg
        .trim()
        .split_once(" agent_rx=")
        .with_context(|| format!("agent_rx missing: {msg}"))?;
    let mcu_us = head
        .split_whitespace()
        .last()
        .context("mcu_us")?
        .parse()
        .with_context(|| format!("mcu_us in {msg}"))?;
    let agent_rx = tail
        .trim()
        .parse()
        .with_context(|| format!("agent_rx in {msg}"))?;
    Ok((mcu_us, agent_rx))
}

/// One tcpdump and one 2 ms CASE0 sender for a whole repeat loop.
pub(crate) struct LiveTap {
    stop: Arc<AtomicBool>,
    text: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
    child: tokio::process::Child,
    sender: Option<tokio::task::JoinHandle<Result<()>>>,
    reader: Option<tokio::task::JoinHandle<()>>,
    err_reader: Option<tokio::task::JoinHandle<()>>,
}

impl LiveTap {
    pub(crate) async fn open(cfg: &FileConfig) -> Result<Self> {
        let mut child = tokio::process::Command::new("tcpdump")
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
        let text = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&text);
        let reader = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        sink.lock().unwrap().push_str(&chunk);
                    }
                }
            }
        });
        let err = Arc::new(Mutex::new(String::new()));
        let err_sink = Arc::clone(&err);
        let err_reader = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        err_sink.lock().unwrap().push_str(&chunk);
                    }
                }
            }
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
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(Self {
            stop,
            text,
            err,
            child,
            sender: Some(sender),
            reader: Some(reader),
            err_reader: Some(err_reader),
        })
    }

    pub(crate) fn count_before(&self, mac: &str, needle: &str, before: f64) -> usize {
        frame_times(&self.text.lock().unwrap(), mac, needle)
            .into_iter()
            .filter(|ts| *ts < before)
            .count()
    }

    /// Byte offset into the capture buffer. Take it before the command so the
    /// rows stored for this repeat include the frames that came before the
    /// switch as well as the one that proves it.
    pub(crate) fn mark(&self) -> usize {
        self.text.lock().unwrap().len()
    }

    /// Captured stdout from `mark` on, starting at a whole line. Frames whose
    /// header began before the mark are dropped, so every block is complete.
    pub(crate) fn capture(&self, mark: usize) -> String {
        let text = self.text.lock().unwrap();
        let mut start = mark.min(text.len());
        if start > 0 && !text.is_char_boundary(start) {
            return String::new();
        }
        if start > 0 && !text[..start].ends_with('\n') {
            match text[start..].find('\n') {
                Some(nl) => start += nl + 1,
                None => return String::new(),
            }
        }
        let mut out = text[start..].to_string();
        // The reader may be mid-packet at snapshot time. Keep whole lines only.
        if !out.ends_with('\n')
            && let Some(nl) = out.rfind('\n')
        {
            out.truncate(nl + 1);
        }
        out
    }

    /// Drop everything before `mark` so polling stays bounded over 100 repeats.
    pub(crate) fn trim(&self, mark: usize) {
        let mut text = self.text.lock().unwrap();
        let mut cut = mark.min(text.len());
        if cut > 0 && !text.is_char_boundary(cut) {
            return;
        }
        if cut > 0 && !text[..cut].ends_with('\n') {
            match text[cut..].find('\n') {
                Some(nl) => cut += nl + 1,
                None => return,
            }
        }
        text.drain(..cut);
    }

    /// tcpdump diagnostics, if it wrote any. Errors here explain a missing frame.
    pub(crate) fn stderr_text(&self) -> String {
        self.err.lock().unwrap().clone()
    }

    pub(crate) async fn wait_frame(&self, mac: &str, needle: &str, after: f64) -> Result<f64> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(ts) = frame_times(&self.text.lock().unwrap(), mac, needle)
                .into_iter()
                .filter(|ts| *ts >= after)
                .reduce(f64::min)
            {
                return Ok(ts);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("no DUT frame {needle} at or after {after:.6}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub(crate) async fn close(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(sender) = self.sender.take() {
            let _ = sender.await;
        }
        self.child.start_kill().ok();
        let _ = self.child.wait().await;
        if let Some(reader) = self.reader.take() {
            let _ = reader.await;
        }
        if let Some(err_reader) = self.err_reader.take() {
            let _ = err_reader.await;
        }
        Ok(())
    }
}

/// An early `?` must not leave the 2 ms sender and tcpdump running into the
/// next exam of the same process. `close` is still the orderly path.
impl Drop for LiveTap {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.child.start_kill().ok();
    }
}

fn frame_times(dump: &str, mac: &str, needle: &str) -> Vec<f64> {
    let src = format!("{mac} >");
    let mut out = Vec::new();
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
        if let Some(ts) = frame_ts(header) {
            out.push(ts);
        }
    }
    out
}

async fn run_cold_latency(
    cfg: &FileConfig,
    agent: &AgentClient,
    client: &tokio_postgres::Client,
    exam_id: db::ExamId,
) -> Result<()> {
    agent.clear_xdp("*").await.context("clear cold table")?;
    agent.flash_xdp("case_swap").await.context("install case_swap")?;
    agent.flash_xdp("case_mark").await.context("install case_mark")?;
    agent.get_init().await.context("OK init after both images")?;
    agent.disable_xdp().await.context("drop the lease before the first fill")?;
    let _ = agent
        .flash_xdp_slot("case_swap", 0)
        .await
        .context("copy case_swap into slot 0")?;
    agent.enable_xdp().await.context("lease slot 0")?;
    let mac = format_mac(&cfg.esp_mac_bytes()?);
    let tap = LiveTap::open(cfg).await?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let armed = unix_secs();
    if tap.count_before(&mac, DST_A, armed + 1.0) == 0 {
        let _ = tap.close().await;
        bail!("cold latency did not see destination {DST_A} before the first copy");
    }
    for rep in 1..=REPEATS {
        let (name, dst) = if rep % 2 == 1 {
            ("case_mark", MARK_DST)
        } else {
            ("case_swap", DST_A)
        };
        let start = unix_secs();
        let mark = tap.mark();
        let reply = agent
            .flash_xdp_slot(name, 0)
            .await
            .with_context(|| format!("cold rep {rep} flash {name}"))?;
        let (mcu_us, agent_rx) = reply_stamps(&reply)?;
        let end = tap.wait_frame(&mac, dst, start).await.with_context(|| {
            format!("cold rep {rep} end {dst}\n{reply}")
        })?;
        // start and end are the test process clock. agent_rx is the agent host
        // clock and is stored as an absolute value, not subtracted from start.
        let end_ms = (end - start) * 1_000.0;
        eprintln!(
            "STAGE cold rep={rep}/{REPEATS} image={name} mcu_us={mcu_us} start={start:.6} agent_rx={agent_rx:.6} end_ms={end_ms:.3}"
        );
        let window = tap.capture(mark);
        tap.trim(mark);
        let mut report = format!(
            "# dut_mac {mac}\n# image {name}\n# start={start:.6} mcu_us={mcu_us} agent_rx={agent_rx:.6} end={end:.6}\n{reply}\n{window}"
        );
        let err = tap.stderr_text();
        if !err.is_empty() {
            report.push_str("\n# tcpdump stderr\n");
            report.push_str(&err);
        }
        let report = report;
        db::insert_case_obs(
            client,
            exam_id,
            &CaseObs {
                seq: 10 + rep as i32 - 1,
                xdp: name,
                capability: "cold slot copy",
                action: "XDP_TX",
                expect: "start is the test clock when flash-to-slot is issued. mcu_us is the device clock when the new bytes are published; the quiesce is cleared immediately after that store. agent_rx is the agent host clock when the MCU reply is read, stored absolute and not subtracted from start. end is the first DUT frame with the new destination at or after start, on the test clock. The cold table is not erased and the DUT does not reboot inside the loop.",
                map_name: "",
                map_slot: 0,
                map_before: 0,
                map_after: 0,
                sent: SENT as i32,
                saw_reply: true,
                ok: true,
                tcpdump: &report,
            },
        )
        .await?;
    }
    tap.close().await?;
    Ok(())
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
