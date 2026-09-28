//! Whitelist DDoS loss bench (`test wl-loss`).
//!
//! Fixed legit interval (2.2 ms); sweeps α×100 including 0 (no-attack baseline).
//! Probe sends a fixed random byte stream (whitelist only). `hping3` interferes
//! when α>0. MCU collects bytes; dump returns them for loss %.
//! Postgres: `xdp_loss_config` / `xdp_loss_data`.

mod pg;

use std::cell::Cell;
use std::io::{self, IsTerminal, Write};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use log::{debug, info, warn};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use test_common::hot_load_op::{
    self, DATAPATH_DISABLE, DATAPATH_ENABLE, HotLoadOp, KIND_DATAPATH, KIND_READBACK,
    STORAGE_SRAM_A, swap_rx_op,
};
use test_common::map_crud_op::{self, MapCrudOp};
use test_common::{COLLECT_CAP, COLLECT_HDR_LEN, MAGIC_COLLECT};

use crate::harness::Harness;

use super::hping::Hping;
use super::pg_dsn::PgOpts;
use super::reset::reboot_dut;
use super::stop::{self, Stop};
use super::wl_common::{
    self, AlphaTask, INTERFERER, LEGIT_INTERVAL_US, alpha_tasks, fmt_dur, hping_pps, ip_key,
    pick_peers,
};
use pg::{LossRow, Pg};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/ddos_wl_assets.rs"));
}

const MAP_NAME: &str = "ddos_wl";
const SEND_WINDOW: Duration = Duration::from_secs(10);
const SMOKE_WINDOW: Duration = Duration::from_secs(3);
const READY_POLL: Duration = Duration::from_secs(90);
const POST_READY: Duration = Duration::from_secs(30);
const U_ECHO_WAIT: Duration = Duration::from_millis(100);
const U_ECHO_STREAK: u32 = 10;
const YIELD_EVERY: u32 = 32;
const MAP_KEY_SIZE: u32 = 4;
const MAP_VAL_SIZE: u32 = 1;
const MAP_MAX_ENTRIES: u32 = 64;
const UDP_PAYLOAD: usize = 64;
const STREAM_LEN: usize = COLLECT_CAP;
const DUMP_SETTLE: Duration = Duration::from_millis(500);
const DUMP_RETRIES: u32 = 5;
const DUMP_RETRY_GAP: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug)]
struct Task {
    alpha_x100: u32,
}

impl Task {
    fn interval_us(self) -> u32 {
        LEGIT_INTERVAL_US
    }
}

#[derive(Clone)]
struct Peers {
    ctrl: Ipv4Addr,
    wl: Vec<Ipv4Addr>,
    atk: Vec<Ipv4Addr>,
}

impl From<wl_common::Peers> for Peers {
    fn from(p: wl_common::Peers) -> Self {
        Self {
            ctrl: p.ctrl,
            wl: p.wl,
            atk: p.atk,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RoundStats {
    pub sent_pkts: usize,
    pub sent_bytes: usize,
    pub recv_bytes: usize,
    pub match_bytes: usize,
    pub loss_pct: Option<f64>,
    pub dump_ok: bool,
    pub dump_attempts: u32,
    pub hping_pps: u32,
    pub config_id: Option<i64>,
}

fn gen_stream(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    // Never 0: MCU dump uses 0 for “never written”.
    (0..len).map(|_| rng.random_range(1u8..=255)).collect()
}

fn build_collect_pkt(index: u16, data: u8, pad_len: usize) -> Vec<u8> {
    let mut p = Vec::with_capacity(COLLECT_HDR_LEN + pad_len);
    p.extend_from_slice(&MAGIC_COLLECT);
    p.extend_from_slice(&index.to_le_bytes());
    p.push(data);
    p.resize(COLLECT_HDR_LEN + pad_len, 0);
    p
}

fn match_stream(sent: &[u8], recv: &[u8]) -> usize {
    sent.iter()
        .zip(recv.iter().chain(std::iter::repeat(&0u8)))
        .take(sent.len())
        .filter(|(a, b)| a == b)
        .count()
}

pub async fn run_wl_loss(harness: &mut Harness, smoke: bool, formal: bool) -> Result<()> {
    let stop = Stop::new();
    stop::spawn_watch(stop.clone());

    let opts = PgOpts { formal };
    let Some(mut pg) = Pg::connect_retry(opts, &stop).await else {
        eprintln!("stopped before postgres");
        return Ok(());
    };

    let tasks: Vec<Task> = alpha_tasks(smoke)
        .into_iter()
        .map(|AlphaTask { alpha_x100 }| Task { alpha_x100 })
        .collect();
    let total = tasks.len();
    let window = if smoke { SMOKE_WINDOW } else { SEND_WINDOW };

    eprintln!(
        "wl-loss {total} rounds  interval={LEGIT_INTERVAL_US}us × α×100 {:?}  stream={STREAM_LEN}  → xdp_loss_*  formal={formal}",
        tasks.iter().map(|t| t.alpha_x100).collect::<Vec<_>>()
    );
    eprintln!("foreground SSH; Ctrl+C finishes the current round (ingest, no extra reboot)");

    let peers: Peers = pick_peers(harness.esp_ip).into();
    info!(
        "wl-loss peers ctrl={} wl={:?} atk={:?} esp={} smoke={smoke}",
        peers.ctrl, peers.wl, peers.atk, harness.esp_ip
    );

    let mut need_reboot = false;
    let mut done = 0usize;
    let started = Instant::now();
    let bar = Bar::new(Some(total));

    for task in tasks {
        if stop.is_set() {
            break;
        }
        bar.begin_round(done + 1, &task);
        let stats = exec_round(
            harness,
            &peers,
            &task,
            window,
            POST_READY,
            &stop,
            need_reboot,
            Some(&mut pg),
            Some(&bar),
        )
        .await?;
        if stop.is_set() && stats.sent_pkts == 0 {
            bar.clear_live();
            break;
        }
        if need_reboot && stats.sent_pkts == 0 {
            bar.clear_live();
            eprintln!("DUT did not come back; stopping wl-loss");
            break;
        }
        done += 1;
        bar.end_round(&task, &stats);
        need_reboot = true;
        if stop.is_set() {
            break;
        }
    }
    eprintln!(
        "wl-loss done  {done}/{total}  elapsed {}",
        fmt_dur(started.elapsed())
    );
    Ok(())
}

const BAR_W: usize = 20;

struct Bar {
    total: Option<usize>,
    tty: bool,
    round: Cell<usize>,
    painted: Cell<bool>,
    last_paint: Cell<Option<Instant>>,
    send_n: Cell<u32>,
    send_exp: Cell<u32>,
}

impl Bar {
    fn new(total: Option<usize>) -> Self {
        Self {
            total,
            tty: io::stderr().is_terminal(),
            round: Cell::new(0),
            painted: Cell::new(false),
            last_paint: Cell::new(None),
            send_n: Cell::new(0),
            send_exp: Cell::new(0),
        }
    }

    fn clear_live(&self) {
        if self.tty && self.painted.get() {
            eprint!("\x1b[2A\x1b[0J");
            let _ = io::stderr().flush();
            self.painted.set(false);
        }
    }

    fn no_label(&self) -> String {
        match self.total {
            Some(t) => format!("No. {}/{t}", self.round.get()),
            None => format!("No. {}", self.round.get()),
        }
    }

    fn begin_round(&self, n: usize, task: &Task) {
        self.round.set(n);
        self.send_n.set(0);
        self.send_exp.set(STREAM_LEN as u32);
        self.last_paint.set(None);
        self.paint(task, "setup");
    }

    fn phase(&self, task: &Task, phase: &str) {
        self.paint(task, phase);
    }

    fn send_tick(&self, task: &Task, sent: u32) {
        self.send_n.set(sent);
        self.paint_throttled(task, "send");
    }

    fn glyph(n: u32, den: u32) -> String {
        let filled = if den == 0 {
            0
        } else {
            ((n as u64 * BAR_W as u64) / u64::from(den)).min(BAR_W as u64) as usize
        };
        format!("[{}{}]", "=".repeat(filled), " ".repeat(BAR_W - filled))
    }

    fn paint_throttled(&self, task: &Task, phase: &str) {
        let now = Instant::now();
        let send_n = self.send_n.get();
        let send_exp = self.send_exp.get();
        let force = send_n == 1 || (send_exp > 0 && send_n == send_exp);
        if !force
            && self
                .last_paint
                .get()
                .is_some_and(|t| now.duration_since(t) < Duration::from_millis(100))
        {
            return;
        }
        self.paint(task, phase);
    }

    fn paint(&self, task: &Task, phase: &str) {
        let pps = hping_pps(task.alpha_x100);
        let h = format!(
            "{}  {:>5}us  α×100={:<3}  hping≈{pps}  {phase}",
            self.no_label(),
            task.interval_us(),
            task.alpha_x100,
        );
        let send_n = self.send_n.get();
        let send_exp = self.send_exp.get();
        let send = format!(
            "stream {}  {send_n}/{send_exp}",
            Self::glyph(send_n, send_exp.max(1))
        );
        if self.tty {
            if self.painted.get() {
                eprint!("\x1b[2A");
            }
            eprint!("\x1b[2K\r{h}\n\x1b[2K{send}\n");
            let _ = io::stderr().flush();
            self.painted.set(true);
            self.last_paint.set(Some(Instant::now()));
        }
    }

    fn end_round(&self, task: &Task, stats: &RoundStats) {
        if self.tty && self.painted.get() {
            eprint!("\x1b[2A");
        }
        let loss = if stats.dump_ok {
            format!(
                "recv {}B match {}/{} loss={:.1}%",
                stats.recv_bytes,
                stats.match_bytes,
                stats.sent_bytes,
                stats.loss_pct.unwrap_or(0.0)
            )
        } else {
            format!(
                "DUMP_FAIL attempts={} recv {}B",
                stats.dump_attempts, stats.recv_bytes
            )
        };
        eprintln!(
            "{}  {}us  α×100={}  hping={}pps  {loss}  pg={:?}",
            self.no_label(),
            task.interval_us(),
            task.alpha_x100,
            stats.hping_pps,
            stats.config_id
        );
        self.painted.set(false);
    }
}

async fn exec_round(
    harness: &mut Harness,
    peers: &Peers,
    task: &Task,
    window: Duration,
    post_ready: Duration,
    stop: &Stop,
    mut need_reboot: bool,
    mut pg: Option<&mut Pg>,
    bar: Option<&Bar>,
) -> Result<RoundStats> {
    loop {
        if stop.is_set() {
            info!("bench round stop before send");
            return Ok(RoundStats::default());
        }
        if need_reboot {
            if let Some(bar) = bar {
                bar.phase(task, "reboot");
            }
            if !reboot_until_ready(harness, peers.ctrl, stop, bar, task).await {
                info!("bench round stop while waiting for DUT");
                return Ok(RoundStats::default());
            }
            if !wait_u_echo(harness, peers.ctrl, post_ready, stop, bar, task).await {
                if stop.is_set() {
                    info!("bench round stop during post-ready wait");
                    return Ok(RoundStats::default());
                }
                warn!("U echo not stable in {:?}; reboot", post_ready);
                need_reboot = true;
                continue;
            }
        }
        if let Some(bar) = bar {
            bar.phase(task, "reconcile");
        }
        match reconcile(harness, peers).await {
            Ok(()) => break,
            Err(e) => {
                warn!("bench reconcile failed ({e:#}); reboot and retry");
                need_reboot = true;
            }
        }
    }

    let pps = hping_pps(task.alpha_x100);
    let stream_seed = rand::rng().random::<u64>();
    let stream = gen_stream(stream_seed, STREAM_LEN);
    let src0 = peers.wl[0];

    // Clear MCU collect buffer via :8081 (always XDP PASS).
    if let Err(e) = harness.collect_reset(src0).await {
        warn!("collect reset: {e:#}; continuing");
    }

    let atk = peers.atk[0];
    let hping = match Hping::start(harness.esp_ip, atk, harness.echo_port, pps) {
        Ok(h) => h,
        Err(e) => {
            warn!("hping3 start failed ({e:#}); continuing without interferer");
            Hping::start(harness.esp_ip, atk, harness.echo_port, 0)?
        }
    };

    let sent = match send_stream(harness, peers, task, &stream, window, stop, bar).await {
        Ok(n) => n,
        Err(e) => {
            warn!("bench send: {e:#}");
            0
        }
    };
    hping.stop();

    if stop.is_set() && sent == 0 {
        return Ok(RoundStats::default());
    }

    if let Some(bar) = bar {
        bar.phase(task, "settle");
    }
    stop.sleep_or(DUMP_SETTLE).await;

    if let Some(bar) = bar {
        bar.phase(task, "dump");
    }

    let mut recv = Vec::new();
    let mut dump_ok = false;
    let mut dump_attempts = 0u32;
    for attempt in 1..=DUMP_RETRIES {
        if stop.is_set() {
            break;
        }
        dump_attempts = attempt;
        match harness.collect_dump(src0, STREAM_LEN as u16).await {
            Ok(v) => {
                recv = v;
                dump_ok = true;
                break;
            }
            Err(e) => {
                warn!("collect dump attempt {attempt}/{DUMP_RETRIES}: {e:#}");
                stop.sleep_or(DUMP_RETRY_GAP).await;
            }
        }
    }

    let (match_bytes, loss_pct) = if dump_ok {
        let m = match_stream(&stream, &recv);
        let loss = if stream.is_empty() {
            0.0
        } else {
            100.0 * (1.0 - m as f64 / stream.len() as f64)
        };
        (m, Some(loss))
    } else {
        (0, None)
    };

    let mut stats = RoundStats {
        sent_pkts: sent,
        sent_bytes: stream.len(),
        recv_bytes: recv.len(),
        match_bytes,
        loss_pct,
        dump_ok,
        dump_attempts,
        hping_pps: pps,
        config_id: None,
    };

    let row = LossRow {
        t0: SystemTime::now(),
        sent_pkts: sent as i32,
        sent_bytes: stream.len() as i32,
        recv_bytes: recv.len() as i32,
        match_bytes: match_bytes as i32,
        loss_pct,
        dump_ok,
        dump_attempts: dump_attempts as i32,
        sent_sha256: None,
        recv_sha256: None,
    };

    if let Some(pg) = pg.as_mut() {
        if let Some(id) = pg
            .ingest_until_ok(
                task.interval_us() as i32,
                STREAM_LEN as i32,
                UDP_PAYLOAD as i32,
                pps as i32,
                window.as_secs() as i32,
                INTERFERER,
                stream_seed as i64,
                &row,
                stop,
            )
            .await
        {
            stats.config_id = Some(id);
            if dump_ok {
                info!(
                    "bench ingest config_id={id} match={match_bytes}/{} loss={:.1}% interval={}us hping_pps={pps}",
                    stream.len(),
                    loss_pct.unwrap_or(0.0),
                    task.interval_us()
                );
            } else {
                info!(
                    "bench ingest config_id={id} DUMP_FAIL attempts={dump_attempts} interval={}us hping_pps={pps}",
                    task.interval_us()
                );
            }
        }
    }

    harness.reset_state().await;
    Ok(stats)
}

async fn send_stream(
    harness: &Harness,
    peers: &Peers,
    task: &Task,
    stream: &[u8],
    window: Duration,
    stop: &Stop,
    bar: Option<&Bar>,
) -> Result<usize> {
    let pad_len = UDP_PAYLOAD.saturating_sub(COLLECT_HDR_LEN);
    let start = Instant::now();
    let deadline = start + window;
    let mut wl_i = 0usize;
    let mut sent = 0usize;

    if task.interval_us() == 0 {
        for (i, &b) in stream.iter().enumerate() {
            if Instant::now() >= deadline || stop.is_set() {
                break;
            }
            let src = peers.wl[wl_i % peers.wl.len()];
            wl_i += 1;
            let pkt = build_collect_pkt(i as u16, b, pad_len);
            harness
                .send_echo_payload(src, &pkt)
                .context("send collect")?;
            sent += 1;
            if sent as u32 % YIELD_EVERY == 0 {
                tokio::task::yield_now().await;
                if let Some(bar) = bar {
                    bar.send_tick(task, sent as u32);
                }
            }
        }
    } else {
        let gap = Duration::from_micros(u64::from(task.interval_us()));
        let mut next = start;
        for (i, &b) in stream.iter().enumerate() {
            if Instant::now() >= deadline || stop.is_set() {
                break;
            }
            let now = Instant::now();
            if now < next {
                stop.sleep_or(next.saturating_duration_since(now)).await;
            }
            if stop.is_set() || Instant::now() >= deadline {
                break;
            }
            let src = peers.wl[wl_i % peers.wl.len()];
            wl_i += 1;
            let pkt = build_collect_pkt(i as u16, b, pad_len);
            harness
                .send_echo_payload(src, &pkt)
                .context("send collect")?;
            sent += 1;
            next += gap;
            if let Some(bar) = bar {
                bar.send_tick(task, sent as u32);
            }
            if sent as u32 % YIELD_EVERY == 0 {
                tokio::task::yield_now().await;
            }
        }
    }
    if let Some(bar) = bar {
        bar.send_tick(task, sent as u32);
    }
    debug!(
        "bench stream sent={sent}/{} interval={}us α×100={} hping_pps={}",
        stream.len(),
        task.interval_us(),
        task.alpha_x100,
        hping_pps(task.alpha_x100)
    );
    Ok(sent)
}

async fn datapath(harness: &Harness, ctrl: Ipv4Addr, region: u32) -> Result<()> {
    let ack = harness
        .hot_load_op(
            ctrl,
            HotLoadOp {
                kind: KIND_DATAPATH,
                region,
                dram_slot: 0,
                src: 0,
                dst: 0,
                payload: &[],
            },
        )
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("DATAPATH region={region} status={}", ack.status);
    }
    Ok(())
}

async fn readback_len_wait(harness: &Harness, ctrl: Ipv4Addr, wait: Duration) -> Result<u32> {
    let ack = harness
        .hot_load_op_wait(
            ctrl,
            HotLoadOp {
                kind: KIND_READBACK,
                region: 0,
                dram_slot: 0,
                src: STORAGE_SRAM_A,
                dst: 0,
                payload: &[],
            },
            wait,
        )
        .await?;
    Ok(ack.readback_len)
}

async fn readback_len(harness: &Harness, ctrl: Ipv4Addr) -> Result<u32> {
    readback_len_wait(harness, ctrl, harness.pair_timeout()).await
}

async fn reboot(harness: &Harness, ctrl: Ipv4Addr, stop: &Stop) {
    reboot_dut(harness, ctrl, "wl-loss", stop).await;
}

async fn wait_ready(
    harness: &Harness,
    ctrl: Ipv4Addr,
    stop: &Stop,
    bar: Option<&Bar>,
    task: &Task,
) -> bool {
    const PROBE: Duration = Duration::from_millis(80);
    let start = Instant::now();
    while start.elapsed() < READY_POLL {
        if stop.is_set() {
            return false;
        }
        if let Some(bar) = bar {
            bar.phase(task, &format!("DUT {:.1}s", start.elapsed().as_secs_f32()));
        }
        if readback_len_wait(harness, ctrl, PROBE).await.is_ok() {
            return true;
        }
    }
    false
}

async fn wait_u_echo(
    harness: &mut Harness,
    src: Ipv4Addr,
    budget: Duration,
    stop: &Stop,
    bar: Option<&Bar>,
    task: &Task,
) -> bool {
    let t0 = Instant::now();
    let mut streak = 0u32;
    while t0.elapsed() < budget {
        if stop.is_set() {
            return false;
        }
        if let Some(bar) = bar {
            bar.phase(task, &format!("hold {streak}/{U_ECHO_STREAK}"));
        }
        let seq = harness.alloc_seq();
        let ok = match harness.send_stimulus(src, None, seq).await {
            Ok(()) => harness
                .wait_echo(seq, Some(U_ECHO_WAIT))
                .await
                .ok()
                .flatten()
                .is_some(),
            Err(_) => false,
        };
        let _ = harness.take_seq(seq).await;
        if ok {
            streak += 1;
            if streak >= U_ECHO_STREAK {
                if let Some(bar) = bar {
                    bar.phase(task, &format!("hold {streak}/{U_ECHO_STREAK}"));
                }
                harness.reset_state().await;
                return true;
            }
        } else {
            streak = 0;
        }
    }
    false
}

async fn reboot_until_ready(
    harness: &Harness,
    ctrl: Ipv4Addr,
    stop: &Stop,
    bar: Option<&Bar>,
    task: &Task,
) -> bool {
    for try_n in 1..=3u32 {
        if stop.is_set() {
            return false;
        }
        reboot(harness, ctrl, stop).await;
        if wait_ready(harness, ctrl, stop, bar, task).await {
            return true;
        }
        warn!("bench DUT not ready in {:?} (try {try_n}/3)", READY_POLL);
    }
    false
}

async fn lookup(
    harness: &Harness,
    ctrl: Ipv4Addr,
    key: &[u8],
) -> Result<test_common::map_crud_op::MapCrudAck<'static>> {
    harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_LOOKUP,
                MAP_NAME,
                map_crud_op::MAP_TYPE_HASH,
                key,
                &[0u8],
                0,
            ),
        )
        .await
}

async fn update(harness: &Harness, ctrl: Ipv4Addr, ip: Ipv4Addr) -> Result<()> {
    let key = ip_key(ip);
    let value = [1u8];
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_UPDATE,
                MAP_NAME,
                map_crud_op::MAP_TYPE_HASH,
                &key,
                &value,
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("UPDATE {ip} status={}", ack.status);
    }
    Ok(())
}

async fn create_map(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp {
                kind: map_crud_op::KIND_CREATE,
                map_name: MAP_NAME,
                map_type: map_crud_op::MAP_TYPE_HASH,
                key: &[],
                value: &[],
                flags: 0,
                key_size: MAP_KEY_SIZE,
                value_size: MAP_VAL_SIZE,
                max_entries: MAP_MAX_ENTRIES,
                aot_addr: assets::AOT_ADDR,
            },
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK && ack.status != map_crud_op::STATUS_CONFLICT {
        bail!("CREATE {MAP_NAME} status={} {}", ack.status, ack.reason);
    }
    Ok(())
}

async fn swap_aot(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    let ack = harness
        .hot_load_op(ctrl, swap_rx_op(0, assets::BIN, 0))
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("SWAP status={} {}", ack.status, ack.reason);
    }
    if ack.loaded_len as usize != assets::BIN.len() {
        bail!(
            "SWAP loaded_len={} want {}",
            ack.loaded_len,
            assets::BIN.len()
        );
    }
    Ok(())
}

fn lookup_ok(ack: &test_common::map_crud_op::MapCrudAck<'_>) -> bool {
    ack.status == map_crud_op::STATUS_OK && !ack.value.is_empty()
}

/// DATAPATH disable → SWAP → CREATE → UPDATE ctrl then W → confirm ctrl → enable.
async fn reconcile(harness: &Harness, peers: &Peers) -> Result<()> {
    let ctrl = peers.ctrl;
    datapath(harness, ctrl, DATAPATH_DISABLE)
        .await
        .context("disable")?;

    let rb = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb as usize != assets::BIN.len() {
        swap_aot(harness, ctrl).await.context("SWAP")?;
        datapath(harness, ctrl, DATAPATH_DISABLE).await.ok();
    }

    let probe = lookup(harness, ctrl, &ip_key(ctrl)).await;
    let map_missing = match &probe {
        Ok(ack) => ack.status == map_crud_op::STATUS_NOT_FOUND,
        Err(e) => bail!("LOOKUP {MAP_NAME}: {e:#}"),
    };
    if map_missing {
        create_map(harness, ctrl).await.context("CREATE")?;
        datapath(harness, ctrl, DATAPATH_DISABLE)
            .await
            .context("disable after CREATE")?;
    }

    update(harness, ctrl, ctrl).await.context("UPDATE ctrl")?;
    for ip in &peers.wl {
        update(harness, ctrl, *ip)
            .await
            .with_context(|| format!("UPDATE wl {ip}"))?;
    }

    let ctrl_ack = lookup(harness, ctrl, &ip_key(ctrl)).await?;
    if !lookup_ok(&ctrl_ack) {
        bail!("confirm ctrl {ctrl} missing from {MAP_NAME}");
    }
    for ip in &peers.wl {
        let ack = lookup(harness, ctrl, &ip_key(*ip)).await?;
        if !lookup_ok(&ack) {
            bail!("confirm wl {ip} missing from {MAP_NAME}");
        }
    }
    for ip in &peers.atk {
        let ack = lookup(harness, ctrl, &ip_key(*ip)).await?;
        if lookup_ok(&ack) {
            bail!("attack ip {ip} unexpectedly in whitelist");
        }
    }

    let rb2 = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb2 as usize != assets::BIN.len() {
        bail!("confirm READBACK len={rb2} want {}", assets::BIN.len());
    }
    info!(
        "bench ctrl ok xdp_len={rb2} map={MAP_NAME} ctrl={} wl_n={} atk_n={}",
        peers.ctrl,
        peers.wl.len(),
        peers.atk.len()
    );

    datapath(harness, ctrl, DATAPATH_ENABLE)
        .await
        .context("enable")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hping_pps_scales() {
        assert_eq!(hping_pps(0), 0);
        assert_eq!(hping_pps(100), wl_common::LEGIT_PPS);
        assert_eq!(hping_pps(200), wl_common::LEGIT_PPS * 2);
    }

    #[test]
    fn match_stream_counts() {
        let sent = vec![1, 2, 3, 4];
        assert_eq!(match_stream(&sent, &[1, 2, 3, 4]), 4);
        assert_eq!(match_stream(&sent, &[1, 0, 3, 4]), 3);
        assert_eq!(match_stream(&sent, &[]), 0);
    }

    #[test]
    fn gen_stream_no_zero() {
        let s = gen_stream(42, 256);
        assert_eq!(s.len(), 256);
        assert!(s.iter().all(|&b| b != 0));
        assert_eq!(gen_stream(42, 16), gen_stream(42, 16));
    }

    #[test]
    fn alpha_tasks_smoke_includes_zero() {
        let t = alpha_tasks(true);
        assert!(t.iter().any(|x| x.alpha_x100 == 0));
        assert!(t.iter().any(|x| x.alpha_x100 > 0));
    }
}
