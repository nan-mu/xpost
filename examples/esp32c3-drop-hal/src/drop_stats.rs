//! Drop-bench UDP control on :8081 — 1-byte request, 24-byte reply.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::Instant;

pub const REQ_LEN: usize = 1;
pub const REPLY_LEN: usize = 24;

pub const OP_QUERY: u8 = 0x00;
pub const OP_RESET: u8 = 0x01;
pub const OP_STOP: u8 = 0x02;
/// Warm-up before `t_first`; keep in sync with `test_common::drop_stats::WARMUP_PKTS`.
const WARMUP_PKTS: u64 = 10_000;
/// Cap `drop_count` / `t_last` milestone; keep in sync with `test_common::drop_stats::CAP_PKTS`.
const CAP_PKTS: u64 = 100_000;

#[derive(Clone, Copy)]
struct Stats {
    drop_count: u64,
    t_first: u64,
    t_last: u64,
    drop_enabled: bool,
}

static STATS: Mutex<Cell<Stats>> = Mutex::new(Cell::new(Stats {
    drop_count: 0,
    t_first: 0,
    t_last: 0,
    drop_enabled: false,
}));

fn now_us() -> u64 {
    Instant::now().as_micros()
}

fn with_stats<R>(f: impl FnOnce(&mut Stats) -> R) -> R {
    critical_section::with(|cs| {
        let cell = STATS.borrow(cs);
        let mut s = cell.get();
        let r = f(&mut s);
        cell.set(s);
        r
    })
}

fn note_milestones(prev: u64, count: u64, now: u64, t_first: &mut u64, t_last: &mut u64) {
    if prev < WARMUP_PKTS && count >= WARMUP_PKTS {
        *t_first = now;
    }
    if prev < CAP_PKTS && count >= CAP_PKTS {
        *t_last = now;
    }
}

fn seal_t_last(count: u64, now: u64, t_last: &mut u64) {
    if count >= WARMUP_PKTS && count < CAP_PKTS && *t_last == 0 {
        *t_last = now;
    }
}

/// Whether the UDP 丢包路径 is active (count + discard).
#[inline]
pub fn drop_enabled() -> bool {
    critical_section::with(|cs| STATS.borrow(cs).get().drop_enabled)
}

/// Clear counters and arm the drop path (host RESET).
pub fn reset_and_enable() {
    with_stats(|s| {
        *s = Stats {
            drop_count: 0,
            t_first: 0,
            t_last: 0,
            drop_enabled: true,
        };
    });
}

/// Host STOP: disable path and stamp `t_last` when below `CAP_PKTS`.
pub fn stop_and_seal() {
    let now = now_us();
    with_stats(|s| {
        seal_t_last(s.drop_count, now, &mut s.t_last);
        s.drop_enabled = false;
    });
}

/// Count one inbound UDP frame on the HAL 丢包路径.
pub fn record_udp_drop() {
    with_stats(|s| {
        if !s.drop_enabled || s.drop_count >= CAP_PKTS {
            return;
        }
        let prev = s.drop_count;
        s.drop_count = prev.saturating_add(1);
        note_milestones(prev, s.drop_count, now_us(), &mut s.t_first, &mut s.t_last);
    });
}

pub fn try_handle(pkt: &[u8], out: &mut [u8]) -> Option<usize> {
    if pkt.len() != REQ_LEN {
        return None;
    }
    match pkt[0] {
        OP_RESET => {
            reset_and_enable();
            return None;
        }
        OP_STOP => {
            stop_and_seal();
            return None;
        }
        OP_QUERY => {}
        _ => return None,
    }
    encode_reply(out)
}

fn encode_reply(out: &mut [u8]) -> Option<usize> {
    if out.len() < REPLY_LEN {
        return None;
    }
    let s = critical_section::with(|cs| STATS.borrow(cs).get());
    write_u64(&mut out[0..8], s.drop_count);
    write_u64(&mut out[8..16], s.t_first);
    write_u64(&mut out[16..24], s.t_last);
    Some(REPLY_LEN)
}

fn write_u64(buf: &mut [u8], v: u64) {
    buf[..8].copy_from_slice(&v.to_le_bytes());
}
