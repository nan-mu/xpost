//! Hand-rolled codec for [`proto/map_crud_op.proto`](../../../proto/map_crud_op.proto).

pub const KIND_UPDATE: u32 = 1;
pub const KIND_DELETE: u32 = 2;
pub const KIND_LOOKUP: u32 = 3;
pub const KIND_CREATE: u32 = 4;

pub const MAP_TYPE_HASH: u32 = 1;
pub const MAP_TYPE_ARRAY: u32 = 2;
pub const MAP_TYPE_PROG_ARRAY: u32 = 3;

pub const STATUS_OK: u32 = 0;
pub const STATUS_BAD: u32 = 1;

const MAX_NAME: usize = 32;
const MAX_KEY: usize = 64;
const MAX_VAL: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapCrudOp<'a> {
    pub kind: u32,
    pub map_name: &'a str,
    pub map_type: u32,
    pub key: &'a [u8],
    pub value: &'a [u8],
    pub flags: u64,
    pub key_size: u32,
    pub value_size: u32,
    pub max_entries: u32,
    pub aot_addr: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapCrudAck<'a> {
    pub kind: u32,
    pub status: u32,
    pub value: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadWire,
    UnknownKind,
    FieldTooLarge,
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

fn known_kind(v: u32) -> bool {
    matches!(v, KIND_UPDATE | KIND_DELETE | KIND_LOOKUP | KIND_CREATE)
}

pub fn decode_op(buf: &[u8]) -> Result<MapCrudOp<'_>, DecodeError> {
    let mut i = 0usize;
    let mut kind: Option<u32> = None;
    let mut map_name: &str = "";
    let mut map_type: u32 = 0;
    let mut key: &[u8] = &[];
    let mut value: &[u8] = &[];
    let mut flags: u64 = 0;
    let mut key_size: u32 = 0;
    let mut value_size: u32 = 0;
    let mut max_entries: u32 = 0;
    let mut aot_addr: u64 = 0;

    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => {
                let v = read_varint(buf, &mut i)? as u32;
                if !known_kind(v) {
                    return Err(DecodeError::UnknownKind);
                }
                kind = Some(v);
            }
            (2, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > MAX_NAME {
                    return Err(DecodeError::FieldTooLarge);
                }
                if i + n > buf.len() {
                    return Err(DecodeError::Truncated);
                }
                map_name =
                    core::str::from_utf8(&buf[i..i + n]).map_err(|_| DecodeError::BadWire)?;
                i += n;
            }
            (3, 0) => {
                map_type = read_varint(buf, &mut i)? as u32;
            }
            (4, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > MAX_KEY {
                    return Err(DecodeError::FieldTooLarge);
                }
                if i + n > buf.len() {
                    return Err(DecodeError::Truncated);
                }
                key = &buf[i..i + n];
                i += n;
            }
            (5, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > MAX_VAL {
                    return Err(DecodeError::FieldTooLarge);
                }
                if i + n > buf.len() {
                    return Err(DecodeError::Truncated);
                }
                value = &buf[i..i + n];
                i += n;
            }
            (6, 0) => {
                flags = read_varint(buf, &mut i)?;
            }
            (7, 0) => {
                key_size = read_varint(buf, &mut i)? as u32;
            }
            (8, 0) => {
                value_size = read_varint(buf, &mut i)? as u32;
            }
            (9, 0) => {
                max_entries = read_varint(buf, &mut i)? as u32;
            }
            (10, 0) => {
                aot_addr = read_varint(buf, &mut i)?;
            }
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(MapCrudOp {
        kind: kind.ok_or(DecodeError::BadWire)?,
        map_name,
        map_type,
        key,
        value,
        flags,
        key_size,
        value_size,
        max_entries,
        aot_addr,
    })
}

pub fn encode_ack(ack: &MapCrudAck<'_>, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, (1u64 << 3) | 0)?;
    i = put_varint(out, i, ack.kind as u64)?;
    i = put_varint(out, i, (2u64 << 3) | 0)?;
    i = put_varint(out, i, ack.status as u64)?;
    if !ack.value.is_empty() {
        i = put_varint(out, i, (3u64 << 3) | 2)?;
        i = put_varint(out, i, ack.value.len() as u64)?;
        if i + ack.value.len() > out.len() {
            return None;
        }
        out[i..i + ack.value.len()].copy_from_slice(ack.value);
        i += ack.value.len();
    }
    Some(i)
}

/// Encode `MapCrudOp` (host-side encoder).
pub fn encode_op(op: &MapCrudOp<'_>, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, (1u64 << 3) | 0)?;
    i = put_varint(out, i, op.kind as u64)?;
    i = put_varint(out, i, (2u64 << 3) | 2)?;
    i = put_varint(out, i, op.map_name.len() as u64)?;
    if i + op.map_name.len() > out.len() {
        return None;
    }
    out[i..i + op.map_name.len()].copy_from_slice(op.map_name.as_bytes());
    i += op.map_name.len();
    i = put_varint(out, i, (3u64 << 3) | 0)?;
    i = put_varint(out, i, op.map_type as u64)?;
    if !op.key.is_empty() {
        i = put_varint(out, i, (4u64 << 3) | 2)?;
        i = put_varint(out, i, op.key.len() as u64)?;
        if i + op.key.len() > out.len() {
            return None;
        }
        out[i..i + op.key.len()].copy_from_slice(op.key);
        i += op.key.len();
    }
    if !op.value.is_empty() {
        i = put_varint(out, i, (5u64 << 3) | 2)?;
        i = put_varint(out, i, op.value.len() as u64)?;
        if i + op.value.len() > out.len() {
            return None;
        }
        out[i..i + op.value.len()].copy_from_slice(op.value);
        i += op.value.len();
    }
    if op.flags != 0 {
        i = put_varint(out, i, (6u64 << 3) | 0)?;
        i = put_varint(out, i, op.flags)?;
    }
    if op.key_size != 0 {
        i = put_varint(out, i, (7u64 << 3) | 0)?;
        i = put_varint(out, i, op.key_size as u64)?;
    }
    if op.value_size != 0 {
        i = put_varint(out, i, (8u64 << 3) | 0)?;
        i = put_varint(out, i, op.value_size as u64)?;
    }
    if op.max_entries != 0 {
        i = put_varint(out, i, (9u64 << 3) | 0)?;
        i = put_varint(out, i, op.max_entries as u64)?;
    }
    if op.aot_addr != 0 {
        i = put_varint(out, i, (10u64 << 3) | 0)?;
        i = put_varint(out, i, op.aot_addr)?;
    }
    Some(i)
}

/// Decode `MapCrudAck` (host-side decoder).
pub fn decode_ack(buf: &[u8]) -> Result<MapCrudAck<'_>, DecodeError> {
    let mut i = 0usize;
    let mut kind: Option<u32> = None;
    let mut status: u32 = 0;
    let mut value: &[u8] = &[];
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => kind = Some(read_varint(buf, &mut i)? as u32),
            (2, 0) => status = read_varint(buf, &mut i)? as u32,
            (3, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if n > MAX_VAL {
                    return Err(DecodeError::FieldTooLarge);
                }
                if i + n > buf.len() {
                    return Err(DecodeError::Truncated);
                }
                value = &buf[i..i + n];
                i += n;
            }
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(MapCrudAck {
        kind: kind.ok_or(DecodeError::BadWire)?,
        status,
        value,
    })
}
