//! Drop-stats UDP :8081 — milestones in XDP maps.
//!
//! Bench-done / idle via `dut_evt` ringbuf deferred until feature `ringbuf` is implemented.

use core::cell::RefCell;

use critical_section::Mutex;
use xpost::map::{MAP_RT, MapMutGuard};

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

/// Async ctrl task: send `OP_BENCH_DONE` notify to probe if posted.
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

fn peek_array_u64(name: &str, key: u32) -> Option<u64> {
    let lock = MAP_RT.try_get()?;
    let guard = lock.try_lock().ok()?;
    let id = guard.find(name).ok()?;
    let map = guard.get(id).ok()?;
    let data = map.as_array().ok()?;
    let off = (key as usize).checked_mul(8)?;
    let bytes = data.get(off..off + 8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

fn zero_array_u64(name: &str, key: u32) {
    let Some(lock) = MAP_RT.try_get() else {
        return;
    };
    let Ok(mut guard) = lock.try_lock() else {
        return;
    };
    let Ok(id) = guard.find(name) else {
        return;
    };
    let _ = guard.update(id, |m: &mut MapMutGuard<'_>| -> Result<(), xpost::map::Error> {
        let data = m.as_array().map_err(|_| xpost::map::Error::TypeMismatch)?;
        let off = (key as usize).checked_mul(8).ok_or(xpost::map::Error::NotFound)?;
        let slot = data
            .get_mut(off..off + 8)
            .ok_or(xpost::map::Error::NotFound)?;
        slot.copy_from_slice(&0u64.to_le_bytes());
        Ok(())
    });
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

/// Clear BPF maps used by the drop bench.
///
/// Datapath disable/enable and `dut_evt` hooks stay deferred until `ringbuf` lands.
pub async fn reset_and_enable() {
    critical_section::with(|cs| {
        *PENDING_BENCH_DONE.borrow_ref_mut(cs) = None;
    });
    zero_array_u64("rxcnt", RXCNT_UDP_INDEX);
    zero_array_u64("bench_ts", 0);
    zero_array_u64("bench_ts", 1);
    let _ = IDLE_US;
}

pub fn stop_and_seal() {
    // Datapath disable deferred until public datapath API is wired in this example.
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
