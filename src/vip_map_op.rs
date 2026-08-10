//! Hand-rolled (no nanopb/prost on freestanding firmware).

/// `VIP_MAP_OP_KIND_SET`
pub const KIND_SET: u32 = 1;
/// `VIP_MAP_OP_KIND_CLEAR`
pub const KIND_CLEAR: u32 = 2;

pub const STATUS_OK: u32 = 0;
pub const STATUS_BAD: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VipMapOp {
    pub kind: u32,
    /// Network-order IPv4 as `u32` (LE load of on-wire octets).
    pub vip_be: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VipMapOpAck {
    pub kind: u32,
    pub vip_be: u32,
    pub status: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadWire,
    UnknownKind,
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

/// Decode `VipMapOp` (unknown fields skipped).
pub fn decode_op(buf: &[u8]) -> Result<VipMapOp, DecodeError> {
    let mut i = 0usize;
    let mut kind: Option<u32> = None;
    let mut vip_be: u32 = 0;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => {
                let v = read_varint(buf, &mut i)? as u32;
                if v != KIND_SET && v != KIND_CLEAR {
                    return Err(DecodeError::UnknownKind);
                }
                kind = Some(v);
            }
            (2, 5) => {
                if i + 4 > buf.len() {
                    return Err(DecodeError::Truncated);
                }
                vip_be = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
                i += 4;
            }
            (_, 0) => {
                let _ = read_varint(buf, &mut i)?;
            }
            (_, 1) => {
                i = i.checked_add(8).ok_or(DecodeError::Truncated)?;
                if i > buf.len() {
                    return Err(DecodeError::Truncated);
                }
            }
            (_, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                i = i.checked_add(n).ok_or(DecodeError::Truncated)?;
                if i > buf.len() {
                    return Err(DecodeError::Truncated);
                }
            }
            (_, 5) => {
                i = i.checked_add(4).ok_or(DecodeError::Truncated)?;
                if i > buf.len() {
                    return Err(DecodeError::Truncated);
                }
            }
            _ => return Err(DecodeError::BadWire),
        }
    }
    let kind = kind.ok_or(DecodeError::BadWire)?;
    if kind == KIND_SET && vip_be == 0 {
        return Err(DecodeError::BadWire);
    }
    Ok(VipMapOp { kind, vip_be })
}

/// Encode `VipMapOp` into `out`; returns length.
pub fn encode_op(op: &VipMapOp, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    // kind: field 1, varint
    i = put_varint(out, i, (1u64 << 3) | 0)?;
    i = put_varint(out, i, op.kind as u64)?;
    if op.kind == KIND_SET {
        // vip_be: field 2, fixed32
        i = put_varint(out, i, (2u64 << 3) | 5)?;
        if i + 4 > out.len() {
            return None;
        }
        out[i..i + 4].copy_from_slice(&op.vip_be.to_le_bytes());
        i += 4;
    }
    Some(i)
}

/// Decode `VipMapOpAck`.
pub fn decode_ack(buf: &[u8]) -> Result<VipMapOpAck, DecodeError> {
    let mut i = 0usize;
    let mut kind: Option<u32> = None;
    let mut vip_be: u32 = 0;
    let mut status: u32 = 0;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => {
                kind = Some(read_varint(buf, &mut i)? as u32);
            }
            (2, 5) => {
                if i + 4 > buf.len() {
                    return Err(DecodeError::Truncated);
                }
                vip_be = u32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
                i += 4;
            }
            (3, 0) => {
                status = read_varint(buf, &mut i)? as u32;
            }
            (_, 0) => {
                let _ = read_varint(buf, &mut i)?;
            }
            (_, 1) => {
                i = i.checked_add(8).ok_or(DecodeError::Truncated)?;
                if i > buf.len() {
                    return Err(DecodeError::Truncated);
                }
            }
            (_, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                i = i.checked_add(n).ok_or(DecodeError::Truncated)?;
                if i > buf.len() {
                    return Err(DecodeError::Truncated);
                }
            }
            (_, 5) => {
                i = i.checked_add(4).ok_or(DecodeError::Truncated)?;
                if i > buf.len() {
                    return Err(DecodeError::Truncated);
                }
            }
            _ => return Err(DecodeError::BadWire),
        }
    }
    Ok(VipMapOpAck {
        kind: kind.ok_or(DecodeError::BadWire)?,
        vip_be,
        status,
    })
}

/// Encode `VipMapOpAck` into `out`; returns length.
pub fn encode_ack(ack: &VipMapOpAck, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, (1u64 << 3) | 0)?;
    i = put_varint(out, i, ack.kind as u64)?;
    i = put_varint(out, i, (2u64 << 3) | 5)?;
    if i + 4 > out.len() {
        return None;
    }
    out[i..i + 4].copy_from_slice(&ack.vip_be.to_le_bytes());
    i += 4;
    i = put_varint(out, i, (3u64 << 3) | 0)?;
    i = put_varint(out, i, ack.status as u64)?;
    Some(i)
}
