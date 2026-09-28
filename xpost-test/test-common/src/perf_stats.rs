//! `PERFST` UDP control API — host codec for XDP micro-benchmark snapshots.

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PerfSnapshot {
    pub status: u8,
    pub iterations: u32,
    pub cycles: u32,
    pub instructions: u32,
    pub ipc_milli: u32,
}

impl PerfSnapshot {
    pub fn ipc(self) -> f64 {
        if self.cycles == 0 {
            0.0
        } else {
            self.instructions as f64 / self.cycles as f64
        }
    }
}

pub fn encode_req(op: u8, out: &mut [u8]) -> Option<usize> {
    if out.len() < REQ_LEN {
        return None;
    }
    out[..6].copy_from_slice(&MAGIC);
    out[6] = op;
    Some(REQ_LEN)
}

pub fn encode_run(iterations: u32, out: &mut [u8]) -> Option<usize> {
    if out.len() < REQ_RUN_LEN {
        return None;
    }
    out[..6].copy_from_slice(&MAGIC);
    out[6] = OP_RUN;
    out[7..11].copy_from_slice(&iterations.to_le_bytes());
    Some(REQ_RUN_LEN)
}

pub fn decode_reply(buf: &[u8]) -> Option<PerfSnapshot> {
    if buf.len() < REPLY_LEN || !buf.starts_with(&MAGIC) {
        return None;
    }
    Some(PerfSnapshot {
        status: buf[6],
        iterations: read_u32(&buf[8..12]),
        cycles: read_u32(&buf[12..16]),
        instructions: read_u32(&buf[16..20]),
        ipc_milli: read_u32(&buf[20..24]),
    })
}

fn read_u32(buf: &[u8]) -> u32 {
    u32::from_le_bytes(buf[..4].try_into().expect("u32"))
}
