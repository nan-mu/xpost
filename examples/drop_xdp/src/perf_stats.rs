//! `PERFST` micro-benchmark control API — static snapshot, no heap, no extra task.
//!
//! Multiplexed on existing UDP `:8081` `ctrl_udp_task` (see `sta.rs`).

use core::cell::Cell;

use critical_section::Mutex;
use xpost::perf::{PerfError, Snapshot, bench_rx_hook};

pub const MAGIC: [u8; 6] = *b"PERFST";

pub const OP_RESET: u8 = 0;
pub const OP_QUERY: u8 = 1;
pub const OP_RUN: u8 = 2;

pub const REQ_LEN: usize = 7;
pub const REQ_RUN_LEN: usize = 11;
pub const REPLY_LEN: usize = 24;

pub const STATUS_OK: u8 = 0;
pub const STATUS_ERR_DATAPATH: u8 = 1;
pub const STATUS_ERR_NO_PROG: u8 = 2;
pub const STATUS_ERR_UNSUPPORTED: u8 = 3;
pub const STATUS_ERR_BAD_ITER: u8 = 4;
pub const STATUS_ERR_NOT_RUN: u8 = 5;

const DEFAULT_ITERATIONS: u32 = 1_000;
const MAX_ITERATIONS: u32 = 50_000;

#[derive(Clone, Copy, Default)]
struct Stored {
    status: u8,
    snap: Snapshot,
}

static LAST: Mutex<Cell<Stored>> = Mutex::new(Cell::new(Stored {
    status: STATUS_ERR_NOT_RUN,
    snap: Snapshot {
        iterations: 0,
        cycles: 0,
        instructions: 0,
    },
}));

pub fn reset() {
    critical_section::with(|cs| {
        LAST.borrow(cs).set(Stored {
            status: STATUS_ERR_NOT_RUN,
            snap: Snapshot::default(),
        });
    });
}

fn store(status: u8, snap: Snapshot) {
    critical_section::with(|cs| {
        LAST.borrow(cs).set(Stored { status, snap });
    });
}

fn perf_error_status(e: PerfError) -> u8 {
    match e {
        PerfError::DatapathActive => STATUS_ERR_DATAPATH,
        PerfError::NoProgram => STATUS_ERR_NO_PROG,
        PerfError::IterationsZero => STATUS_ERR_BAD_ITER,
        PerfError::Unsupported => STATUS_ERR_UNSUPPORTED,
    }
}

fn parse_iterations(pkt: &[u8]) -> u32 {
    if pkt.len() >= REQ_RUN_LEN {
        let n = u32::from_le_bytes([pkt[7], pkt[8], pkt[9], pkt[10]]);
        if n != 0 {
            return n.min(MAX_ITERATIONS);
        }
    }
    DEFAULT_ITERATIONS
}

/// Handle `PERFST` on `:8081`. `RUN` blocks the ctrl task while measuring.
pub fn try_handle(pkt: &[u8], out: &mut [u8]) -> Option<usize> {
    if pkt.len() < REQ_LEN || !pkt.starts_with(&MAGIC) {
        return None;
    }
    match pkt[6] {
        OP_RESET => reset(),
        OP_QUERY => {}
        OP_RUN => {
            let iters = parse_iterations(pkt);
            match bench_rx_hook(iters) {
                Ok(snap) => store(STATUS_OK, snap),
                Err(e) => store(perf_error_status(e), Snapshot::default()),
            }
        }
        _ => return None,
    }
    encode_reply(out)
}

fn encode_reply(out: &mut [u8]) -> Option<usize> {
    if out.len() < REPLY_LEN {
        return None;
    }
    let stored = critical_section::with(|cs| LAST.borrow(cs).get());
    out[..6].copy_from_slice(&MAGIC);
    out[6] = stored.status;
    out[7] = 0;
    write_u32(&mut out[8..12], stored.snap.iterations);
    write_u32(&mut out[12..16], stored.snap.cycles);
    write_u32(&mut out[16..20], stored.snap.instructions);
    write_u32(&mut out[20..24], stored.snap.ipc_milli());
    Some(REPLY_LEN)
}

fn write_u32(buf: &mut [u8], v: u32) {
    buf[..4].copy_from_slice(&v.to_le_bytes());
}
