//! Minimal protobuf wire codec for [`hot_load_op.proto`](../../../../proto/hot_load_op.proto).

pub const KIND_SWAP: u32 = 10;
pub const KIND_HOT_SWAP: u32 = 11;
pub const KIND_UPLOAD: u32 = 20;
pub const KIND_COPY: u32 = 30;
pub const KIND_MOVE: u32 = 31;
pub const KIND_READBACK: u32 = 40;
/// Leave datapath stopped (`region=DATAPATH_DISABLE`) or re-enable (`DATAPATH_ENABLE`).
pub const KIND_DATAPATH: u32 = 50;
/// Software reset after the ack is sent. Empty payload.
pub const KIND_REBOOT: u32 = 60;

pub const REGION_A: u32 = 1;
pub const REGION_B: u32 = 2;
pub const REGION_2: u32 = 3;
pub const REGION_3: u32 = 4;

/// `KIND_DATAPATH` / `region`: stop TX/RX AOT gate (passthrough).
pub const DATAPATH_DISABLE: u32 = 1;
/// `KIND_DATAPATH` / `region`: resume datapath (stays on until DISABLE).
pub const DATAPATH_ENABLE: u32 = 2;
/// `KIND_DATAPATH` / `region`: enable, then firmware auto-disables after
/// `dram_slot` milliseconds (paper Listing 1 blast window).
pub const DATAPATH_ENABLE_FOR: u32 = 3;

pub const STORAGE_SRAM_A: u32 = 1;
pub const STORAGE_SRAM_B: u32 = 2;
pub const STORAGE_DRAM_0: u32 = 3;
pub const STORAGE_DRAM_1: u32 = 4;
pub const STORAGE_SRAM_2: u32 = 5;
pub const STORAGE_SRAM_3: u32 = 6;

pub const STATUS_OK: u32 = 200;
pub const STATUS_BAD: u32 = 400;
pub const STATUS_CONFLICT: u32 = 409;

pub const MAX_PAYLOAD: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotLoadOp<'a> {
    pub kind: u32,
    pub region: u32,
    pub dram_slot: u32,
    pub payload: &'a [u8],
    pub src: u32,
    pub dst: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotLoadOpAck<'a> {
    pub kind: u32,
    pub region: u32,
    pub dram_slot: u32,
    pub status: u32,
    pub stashed_len: u32,
    pub loaded_len: u32,
    pub readback_len: u32,
    pub copied_len: u32,
    pub sample_len: u8,
    pub sample: [u8; 32],
    pub reason: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadWire,
    UnknownKind,
    PayloadTooLarge,
}

fn read_varint(buf: &[u8], i: &mut usize) -> Result<u64, DecodeError> {
    let mut x: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *i >= buf.len() {
            return Err(DecodeError::Truncated);
        }
        let b = buf[*i];
        *i += 1;
        x |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(x);
        }
        shift += 7;
        if shift > 63 {
            return Err(DecodeError::BadWire);
        }
    }
}

fn put_varint(out: &mut [u8], mut i: usize, mut x: u64) -> Option<usize> {
    loop {
        if i >= out.len() {
            return None;
        }
        let b = (x as u8) & 0x7f;
        x >>= 7;
        if x == 0 {
            out[i] = b;
            return Some(i + 1);
        }
        out[i] = b | 0x80;
        i += 1;
    }
}

fn skip_field(buf: &[u8], i: &mut usize, wt: u32) -> Result<(), DecodeError> {
    match wt {
        0 => {
            let _ = read_varint(buf, i)?;
        }
        1 => {
            *i = i.checked_add(8).ok_or(DecodeError::Truncated)?;
            if *i > buf.len() {
                return Err(DecodeError::Truncated);
            }
        }
        2 => {
            let n = read_varint(buf, i)? as usize;
            *i = i.checked_add(n).ok_or(DecodeError::Truncated)?;
            if *i > buf.len() {
                return Err(DecodeError::Truncated);
            }
        }
        5 => {
            *i = i.checked_add(4).ok_or(DecodeError::Truncated)?;
            if *i > buf.len() {
                return Err(DecodeError::Truncated);
            }
        }
        _ => return Err(DecodeError::BadWire),
    }
    Ok(())
}

pub fn decode_op(buf: &[u8]) -> Result<HotLoadOp<'_>, DecodeError> {
    let mut i = 0usize;
    let mut kind: Option<u32> = None;
    let mut region: u32 = REGION_A;
    let mut dram_slot: u32 = 0;
    let mut payload: &[u8] = &[];
    let mut src: u32 = 0;
    let mut dst: u32 = 0;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => {
                let v = read_varint(buf, &mut i)? as u32;
                kind = Some(v);
            }
            (2, 0) => {
                region = read_varint(buf, &mut i)? as u32;
            }
            (3, 0) => {
                dram_slot = read_varint(buf, &mut i)? as u32;
            }
            (4, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > MAX_PAYLOAD {
                    return Err(DecodeError::PayloadTooLarge);
                }
                if i.checked_add(n).map(|e| e > buf.len()).unwrap_or(true) {
                    return Err(DecodeError::Truncated);
                }
                payload = &buf[i..i + n];
                i += n;
            }
            (5, 0) => {
                src = read_varint(buf, &mut i)? as u32;
            }
            (6, 0) => {
                dst = read_varint(buf, &mut i)? as u32;
            }
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(HotLoadOp {
        kind: kind.ok_or(DecodeError::BadWire)?,
        region,
        dram_slot,
        payload,
        src,
        dst,
    })
}

/// 切换: overwrite current rx. `region` is a hot slot (`REGION_*`) or 0.
pub fn swap_rx_op(region: u32, payload: &[u8], dram_slot: u32) -> HotLoadOp<'_> {
    HotLoadOp {
        kind: KIND_SWAP,
        region,
        dram_slot,
        payload,
        src: 0,
        dst: 0,
    }
}

/// 热切换: write `payload` into `region` (must be `call`) and flip rx.
pub fn hot_swap_rx_op(region: u32, payload: &[u8]) -> HotLoadOp<'_> {
    HotLoadOp {
        kind: KIND_HOT_SWAP,
        region,
        dram_slot: 0,
        payload,
        src: 0,
        dst: 0,
    }
}

pub fn encode_op(op: &HotLoadOp<'_>, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, (1u64 << 3) | 0)?;
    i = put_varint(out, i, op.kind as u64)?;
    if op.region != 0 {
        i = put_varint(out, i, (2u64 << 3) | 0)?;
        i = put_varint(out, i, op.region as u64)?;
    }
    if op.dram_slot != 0 {
        i = put_varint(out, i, (3u64 << 3) | 0)?;
        i = put_varint(out, i, op.dram_slot as u64)?;
    }
    if !op.payload.is_empty() {
        i = put_varint(out, i, (4u64 << 3) | 2)?;
        i = put_varint(out, i, op.payload.len() as u64)?;
        if i + op.payload.len() > out.len() {
            return None;
        }
        out[i..i + op.payload.len()].copy_from_slice(op.payload);
        i += op.payload.len();
    }
    if op.src != 0 {
        i = put_varint(out, i, (5u64 << 3) | 0)?;
        i = put_varint(out, i, op.src as u64)?;
    }
    if op.dst != 0 {
        i = put_varint(out, i, (6u64 << 3) | 0)?;
        i = put_varint(out, i, op.dst as u64)?;
    }
    Some(i)
}

pub fn decode_ack(buf: &[u8]) -> Result<HotLoadOpAck<'_>, DecodeError> {
    let mut i = 0usize;
    let mut kind: Option<u32> = None;
    let mut region: u32 = 0;
    let mut dram_slot: u32 = 0;
    let mut status: u32 = 0;
    let mut stashed_len: u32 = 0;
    let mut loaded_len: u32 = 0;
    let mut readback_len: u32 = 0;
    let mut copied_len: u32 = 0;
    let mut sample_len: u8 = 0;
    let mut sample = [0u8; 32];
    let mut reason: &str = "";
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => kind = Some(read_varint(buf, &mut i)? as u32),
            (2, 0) => region = read_varint(buf, &mut i)? as u32,
            (3, 0) => dram_slot = read_varint(buf, &mut i)? as u32,
            (4, 0) => status = read_varint(buf, &mut i)? as u32,
            (5, 0) => stashed_len = read_varint(buf, &mut i)? as u32,
            (6, 0) => loaded_len = read_varint(buf, &mut i)? as u32,
            (7, 0) => readback_len = read_varint(buf, &mut i)? as u32,
            (8, 0) => copied_len = read_varint(buf, &mut i)? as u32,
            (9, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > 32 {
                    return Err(DecodeError::PayloadTooLarge);
                }
                if i.checked_add(n).map(|e| e > buf.len()).unwrap_or(true) {
                    return Err(DecodeError::Truncated);
                }
                sample[..n].copy_from_slice(&buf[i..i + n]);
                sample_len = n as u8;
                i += n;
            }
            (10, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > 32 {
                    return Err(DecodeError::PayloadTooLarge);
                }
                if i.checked_add(n).map(|e| e > buf.len()).unwrap_or(true) {
                    return Err(DecodeError::Truncated);
                }
                reason = core::str::from_utf8(&buf[i..i + n]).map_err(|_| DecodeError::BadWire)?;
                i += n;
            }
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(HotLoadOpAck {
        kind: kind.ok_or(DecodeError::BadWire)?,
        region,
        dram_slot,
        status,
        stashed_len,
        loaded_len,
        readback_len,
        copied_len,
        sample_len,
        sample,
        reason,
    })
}

pub fn encode_ack(ack: &HotLoadOpAck<'_>, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, (1u64 << 3) | 0)?;
    i = put_varint(out, i, ack.kind as u64)?;
    if ack.region != 0 {
        i = put_varint(out, i, (2u64 << 3) | 0)?;
        i = put_varint(out, i, ack.region as u64)?;
    }
    if ack.dram_slot != 0 {
        i = put_varint(out, i, (3u64 << 3) | 0)?;
        i = put_varint(out, i, ack.dram_slot as u64)?;
    }
    i = put_varint(out, i, (4u64 << 3) | 0)?;
    i = put_varint(out, i, ack.status as u64)?;
    if ack.stashed_len != 0 {
        i = put_varint(out, i, (5u64 << 3) | 0)?;
        i = put_varint(out, i, ack.stashed_len as u64)?;
    }
    if ack.loaded_len != 0 {
        i = put_varint(out, i, (6u64 << 3) | 0)?;
        i = put_varint(out, i, ack.loaded_len as u64)?;
    }
    if ack.readback_len != 0 {
        i = put_varint(out, i, (7u64 << 3) | 0)?;
        i = put_varint(out, i, ack.readback_len as u64)?;
    }
    if ack.copied_len != 0 {
        i = put_varint(out, i, (8u64 << 3) | 0)?;
        i = put_varint(out, i, ack.copied_len as u64)?;
    }
    if ack.sample_len != 0 {
        let n = ack.sample_len as usize;
        i = put_varint(out, i, (9u64 << 3) | 2)?;
        i = put_varint(out, i, n as u64)?;
        if i + n > out.len() {
            return None;
        }
        out[i..i + n].copy_from_slice(&ack.sample[..n]);
        i += n;
    }
    if !ack.reason.is_empty() {
        i = put_varint(out, i, (10u64 << 3) | 2)?;
        i = put_varint(out, i, ack.reason.len() as u64)?;
        if i + ack.reason.len() > out.len() {
            return None;
        }
        out[i..i + ack.reason.len()].copy_from_slice(ack.reason.as_bytes());
        i += ack.reason.len();
    }
    Some(i)
}
