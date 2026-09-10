//! Drop-stats UDP :8081 — milestones in XDP maps; completion via `dut_evt` ringbuf.

use core::cell::RefCell;

use critical_section::Mutex;
use xpost::datapath;
use xpost::evt;
use xpost::map::ops::MapOps;
use xpost::map::{peek_array_u64, with_array_map};

pub const REQ_LEN: usize = 1;
pub const REPLY_LEN: usize = 24;

pub const OP_QUERY: u8 = 0x00;
pub const OP_RESET: u8 = 0x01;
pub const OP_STOP: u8 = 0x02;
/// Host notification: bench cap / done (1 + 24-byte stats).
pub const OP_BENCH_DONE: u8 = 0x03;
pub const BENCH_DONE_LEN: usize = 25;

/// `IPPROTO_UDP` — drop AOT indexes `rxcnt` by IP protocol byte.
const RXCNT_UDP_INDEX: u32 = 17;
const BENCH_TS_FIRST: u32 = 0;
const BENCH_TS_LAST: u32 = 1;
const CAP_PKTS: u64 = 100_000;
/// Idle notification window (µs); keep in sync with `test_common::drop_stats::IDLE_WATCHDOG_US`.
pub const IDLE_US: u64 = 5_000_000;

static PENDING_BENCH_DONE: Mutex<RefCell<Option<BenchDone>>> = Mutex::new(RefCell::new(None));

#[derive(Clone, Copy, Debug)]
pub struct BenchDone {
    pub count: u64,
    pub t_first_us: u64,
    pub t_last_us: u64,
}

pub fn install_evt_handlers() {
    evt::set_bench_done_hook(on_bench_done_evt);
    evt::set_idle_hook(on_idle_evt);
}

fn on_bench_done_evt(count: u64, t_first: u64, t_last: u64) {
    datapath::disable();
    critical_section::with(|cs| {
        *PENDING_BENCH_DONE.borrow_ref_mut(cs) = Some(BenchDone {
            count,
            t_first_us: t_first,
            t_last_us: t_last,
        });
    });
    crate::init_log!(
        "[drop_xdp] bench_done count={} t_us={}..{}",
        count,
        t_first,
        t_last
    );
}

fn on_idle_evt() {
    datapath::disable();
    crate::init_log!("[drop_xdp] idle expire");
}

/// Async ctrl task: send `OP_BENCH_DONE` notify to probe if XDP posted bench_done.
pub fn take_bench_done_notify() -> Option<BenchDone> {
    critical_section::with(|cs| PENDING_BENCH_DONE.borrow_ref_mut(cs).take())
}

pub fn encode_bench_done(done: &BenchDone, out: &mut [u8]) -> Option<usize> {
    if out.len() < BENCH_DONE_LEN {
        return None;
    }
    out[0] = OP_BENCH_DONE;
    write_u64(&mut out[1..9], done.count.min(CAP_PKTS));
    write_u64(&mut out[9..17], done.t_first_us);
    write_u64(&mut out[17..25], done.t_last_us);
    Some(BENCH_DONE_LEN)
}

fn read_udp_rxcnt() -> u64 {
    peek_array_u64("rxcnt", RXCNT_UDP_INDEX).unwrap_or(0)
}

fn read_bench_ts(key: u32) -> u64 {
    peek_array_u64("bench_ts", key).unwrap_or(0)
}

fn reported_count(raw: u64) -> u64 {
    raw.min(CAP_PKTS)
}

/// Clear BPF maps; re-enable datapath.
pub async fn reset_and_enable() {
    datapath::disable();
    critical_section::with(|cs| {
        *PENDING_BENCH_DONE.borrow_ref_mut(cs) = None;
    });
    let key = RXCNT_UDP_INDEX.to_le_bytes();
    let zero = 0u64.to_le_bytes();
    let _ = with_array_map("rxcnt", |m| {
        MapOps::update(m, key.as_ptr(), zero.as_ptr(), 0)
    })
    .await;
    for idx in 0..2u32 {
        let k = idx.to_le_bytes();
        let _ = with_array_map("bench_ts", |m| {
            MapOps::update(m, k.as_ptr(), zero.as_ptr(), 0)
        })
        .await;
    }
    let _ = IDLE_US;
    datapath::enable();
}

pub fn stop_and_seal() {
    datapath::disable();
}

pub fn try_handle(pkt: &[u8], out: &mut [u8]) -> Option<usize> {
    if pkt.len() != REQ_LEN {
        return None;
    }
    match pkt[0] {
        OP_QUERY => {}
        _ => return None,
    }
    encode_reply(out)
}

pub async fn handle_reset() {
    reset_and_enable().await;
}

fn encode_reply(out: &mut [u8]) -> Option<usize> {
    if out.len() < REPLY_LEN {
        return None;
    }
    let raw = read_udp_rxcnt();
    let count = reported_count(raw);
    let t_first = read_bench_ts(BENCH_TS_FIRST);
    let t_last = read_bench_ts(BENCH_TS_LAST);
    write_u64(&mut out[0..8], count);
    write_u64(&mut out[8..16], t_first);
    write_u64(&mut out[16..24], t_last);
    Some(REPLY_LEN)
}

fn write_u64(buf: &mut [u8], v: u64) {
    buf[..8].copy_from_slice(&v.to_le_bytes());
}
