//! DROPST UDP control — `docs/fwtest-experiments.md` §5.1.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::Instant;

pub const MAGIC: &[u8; 6] = b"DROPST";
pub const REQ_LEN: usize = 7;
pub const REPLY_LEN: usize = 46;

const OP_RESET: u8 = 0;
const OP_QUERY: u8 = 1;

#[derive(Clone, Copy)]
struct Stats {
    drop_count: u64,
    t_first: u64,
    t_last: u64,
    window_start: u64,
    window_end: u64,
}

static STATS: Mutex<Cell<Stats>> = Mutex::new(Cell::new(Stats {
    drop_count: 0,
    t_first: 0,
    t_last: 0,
    window_start: 0,
    window_end: 0,
}));

fn now_us() -> u64 {
    Instant::now().as_micros() as u64
}

pub fn reset() {
    critical_section::with(|cs| {
        STATS.borrow(cs).set(Stats {
            drop_count: 0,
            t_first: 0,
            t_last: 0,
            window_start: 0,
            window_end: 0,
        });
    });
}

pub fn set_window_start_us() {
    critical_section::with(|cs| {
        let cell = STATS.borrow(cs);
        let mut s = cell.get();
        s.window_start = now_us();
        cell.set(s);
    });
}

pub fn set_window_end_us() {
    critical_section::with(|cs| {
        let cell = STATS.borrow(cs);
        let mut s = cell.get();
        s.window_end = now_us();
        cell.set(s);
    });
}

pub fn try_handle(pkt: &[u8], out: &mut [u8]) -> Option<usize> {
    if pkt.len() < REQ_LEN || !pkt.starts_with(MAGIC) {
        return None;
    }
    match pkt[6] {
        OP_RESET => reset(),
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
    out[..6].copy_from_slice(MAGIC);
    write_u64(&mut out[6..14], s.drop_count);
    write_u64(&mut out[14..22], s.t_first);
    write_u64(&mut out[22..30], s.t_last);
    write_u64(&mut out[30..38], s.window_start);
    write_u64(&mut out[38..46], s.window_end);
    Some(REPLY_LEN)
}

fn write_u64(buf: &mut [u8], v: u64) {
    buf[..8].copy_from_slice(&v.to_le_bytes());
}
