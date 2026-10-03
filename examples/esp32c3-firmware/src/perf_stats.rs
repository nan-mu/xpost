//! PERFST UDP control — `docs/fwtest-experiments.md` §5.2.

use core::cell::RefCell;

use critical_section::Mutex;
use xpost::perf::{self, Snapshot};

pub const MAGIC: &[u8; 6] = b"PERFST";
pub const REQ_LEN: usize = 7;
pub const REQ_RUN_LEN: usize = 11;
pub const REPLY_LEN: usize = 24;

pub const OP_RESET: u8 = 0;
pub const OP_QUERY: u8 = 1;
pub const OP_RUN: u8 = 2;

pub const STATUS_OK: u8 = 0;
pub const STATUS_ERR_DATAPATH: u8 = 1;
pub const STATUS_ERR_NO_PROG: u8 = 2;
pub const STATUS_ERR_UNSUPPORTED: u8 = 3;
pub const STATUS_ERR_BAD_ITER: u8 = 4;
pub const STATUS_ERR_NOT_RUN: u8 = 5;

const DEFAULT_ITERATIONS: u32 = 1000;
const MAX_ITERATIONS: u32 = 50_000;

static LAST: Mutex<RefCell<Option<Stored>>> = Mutex::new(RefCell::new(None));

#[derive(Clone, Copy)]
struct Stored {
    status: u8,
    snap: Snapshot,
}

pub fn try_handle(pkt: &[u8], out: &mut [u8]) -> Option<usize> {
    if pkt.len() < REQ_LEN || !pkt.starts_with(MAGIC) {
        return None;
    }
    let stored = match pkt[6] {
        OP_RESET => {
            critical_section::with(|cs| *LAST.borrow_ref_mut(cs) = None);
            Stored {
                status: STATUS_OK,
                snap: Snapshot::default(),
            }
        }
        OP_QUERY => critical_section::with(|cs| {
            LAST.borrow_ref(cs)
                .as_ref()
                .copied()
                .unwrap_or(Stored {
                    status: STATUS_ERR_NOT_RUN,
                    snap: Snapshot::default(),
                })
        }),
        OP_RUN => {
            let iterations = if pkt.len() >= REQ_RUN_LEN {
                u32::from_le_bytes(pkt[7..11].try_into().ok()?)
            } else {
                DEFAULT_ITERATIONS
            };
            let iterations = if iterations == 0 {
                DEFAULT_ITERATIONS
            } else {
                iterations
            };
            if iterations > MAX_ITERATIONS {
                return encode_reply(
                    &Stored {
                        status: STATUS_ERR_BAD_ITER,
                        snap: Snapshot::default(),
                    },
                    out,
                );
            }
            let stored = match perf::bench_rx_hook(iterations) {
                Ok(s) => Stored {
                    status: STATUS_OK,
                    snap: s,
                },
                Err(perf::PerfError::DatapathActive) => Stored {
                    status: STATUS_ERR_DATAPATH,
                    snap: Snapshot::default(),
                },
                Err(perf::PerfError::NoProgram) => Stored {
                    status: STATUS_ERR_NO_PROG,
                    snap: Snapshot::default(),
                },
                Err(perf::PerfError::Unsupported) => Stored {
                    status: STATUS_ERR_UNSUPPORTED,
                    snap: Snapshot::default(),
                },
                Err(perf::PerfError::IterationsZero) => Stored {
                    status: STATUS_ERR_BAD_ITER,
                    snap: Snapshot::default(),
                },
            };
            critical_section::with(|cs| *LAST.borrow_ref_mut(cs) = Some(stored));
            stored
        }
        _ => return None,
    };
    encode_reply(&stored, out)
}

fn encode_reply(stored: &Stored, out: &mut [u8]) -> Option<usize> {
    if out.len() < REPLY_LEN {
        return None;
    }
    out[..6].copy_from_slice(MAGIC);
    out[6] = stored.status;
    out[7] = 0;
    out[8..12].copy_from_slice(&stored.snap.iterations.to_le_bytes());
    out[12..16].copy_from_slice(&stored.snap.cycles.to_le_bytes());
    out[16..20].copy_from_slice(&stored.snap.instructions.to_le_bytes());
    out[20..24].copy_from_slice(&stored.snap.ipc_milli().to_le_bytes());
    Some(REPLY_LEN)
}
