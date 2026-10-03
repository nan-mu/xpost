//! `DROPST` stats — see `docs/fwtest-experiments.md` §5.1.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::Instant;

pub const MAGIC: [u8; 6] = *b"DROPST";
pub const OP_RESET: u8 = 0;
pub const OP_QUERY: u8 = 1;
pub const REPLY_LEN: usize = 46;

#[derive(Clone, Copy, Default)]
struct Inner {
    drop_count: u64,
    t_first_us: u64,
    t_last_us: u64,
    window_start_us: u64,
    window_end_us: u64,
}

static STATS: Mutex<Cell<Inner>> = Mutex::new(Cell::new(Inner {
    drop_count: 0,
    t_first_us: 0,
    t_last_us: 0,
    window_start_us: 0,
    window_end_us: 0,
}));

pub fn reset() {
    critical_section::with(|cs| STATS.borrow(cs).set(Inner::default()));
}

pub fn record_packet() {
    let us = Instant::now().as_micros() as u64;
    critical_section::with(|cs| {
        let cell = STATS.borrow(cs);
        let mut s = cell.get();
        s.drop_count += 1;
        if s.t_first_us == 0 {
            s.t_first_us = us;
        }
        s.t_last_us = us;
        cell.set(s);
    });
}

/// Handle `DROPST` request; returns reply length.
pub fn try_handle(pkt: &[u8], out: &mut [u8]) -> Option<usize> {
    if pkt.len() < 7 || !pkt.starts_with(&MAGIC) {
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
    out[..6].copy_from_slice(&MAGIC);
    write_u64(&mut out[6..14], s.drop_count);
    write_u64(&mut out[14..22], s.t_first_us);
    write_u64(&mut out[22..30], s.t_last_us);
    write_u64(&mut out[30..38], s.window_start_us);
    write_u64(&mut out[38..46], s.window_end_us);
    Some(REPLY_LEN)
}

fn write_u64(buf: &mut [u8], v: u64) {
    buf[..8].copy_from_slice(&v.to_le_bytes());
}
