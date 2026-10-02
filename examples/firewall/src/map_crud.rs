//! Minimal MapCrudOp / MapCrudAck wire codec (mirrors xpost-test test-common).

pub const KIND_UPDATE: u32 = 1;
pub const KIND_DELETE: u32 = 2;
pub const KIND_LOOKUP: u32 = 3;
pub const KIND_CREATE: u32 = 4;
pub const KIND_DESTROY_MAP: u32 = 5;

pub const MAP_TYPE_HASH: u32 = 1;

pub const STATUS_OK: u32 = 200;
pub const STATUS_BAD: u32 = 400;
pub const STATUS_NOT_FOUND: u32 = 404;
pub const STATUS_CONFLICT: u32 = 409;

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
}

pub struct MapCrudAck<'a> {
    pub kind: u32,
    pub status: u32,
    pub value: &'a [u8],
    pub reason: &'a str,
}

fn read_varint(buf: &[u8], i: &mut usize) -> Option<u64> {
    let mut x: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *i >= buf.len() {
            return None;
        }
        let b = buf[*i];
        *i += 1;
        x |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(x);
        }
        shift += 7;
        if shift > 63 {
            return None;
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

fn skip_field(buf: &[u8], i: &mut usize, wt: u32) -> Option<()> {
    match wt {
        0 => {
            let _ = read_varint(buf, i)?;
            Some(())
        }
        1 => {
            *i = i.checked_add(8)?;
            Some(())
        }
        2 => {
            let n = read_varint(buf, i)? as usize;
            *i = i.checked_add(n)?;
            Some(())
        }
        5 => {
            *i = i.checked_add(4)?;
            Some(())
        }
        _ => None,
    }
}

pub fn decode_op(buf: &[u8]) -> Option<MapCrudOp<'_>> {
    let mut i = 0usize;
    let mut kind = None;
    let mut map_name: &str = "";
    let mut map_type = 0u32;
    let mut key: &[u8] = &[];
    let mut value: &[u8] = &[];
    let mut flags = 0u64;
    let mut key_size = 0u32;
    let mut value_size = 0u32;
    let mut max_entries = 0u32;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => kind = Some(read_varint(buf, &mut i)? as u32),
            (2, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if i + n > buf.len() {
                    return None;
                }
                map_name = core::str::from_utf8(&buf[i..i + n]).ok()?;
                i += n;
            }
            (3, 0) => map_type = read_varint(buf, &mut i)? as u32,
            (4, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if i + n > buf.len() {
                    return None;
                }
                key = &buf[i..i + n];
                i += n;
            }
            (5, 2) => {
                let n = read_varint(buf, &mut i)? as usize;
                if i + n > buf.len() {
                    return None;
                }
                value = &buf[i..i + n];
                i += n;
            }
            (6, 0) => flags = read_varint(buf, &mut i)?,
            (7, 0) => key_size = read_varint(buf, &mut i)? as u32,
            (8, 0) => value_size = read_varint(buf, &mut i)? as u32,
            (9, 0) => max_entries = read_varint(buf, &mut i)? as u32,
            (_, _) => skip_field(buf, &mut i, wt)?,
        }
    }
    Some(MapCrudOp {
        kind: kind?,
        map_name,
        map_type,
        key,
        value,
        flags,
        key_size,
        value_size,
        max_entries,
    })
}

pub fn encode_ack(ack: &MapCrudAck<'_>, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, 1u64 << 3)?;
    i = put_varint(out, i, ack.kind as u64)?;
    i = put_varint(out, i, 2u64 << 3)?;
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
    if !ack.reason.is_empty() {
        i = put_varint(out, i, (4u64 << 3) | 2)?;
        i = put_varint(out, i, ack.reason.len() as u64)?;
        if i + ack.reason.len() > out.len() {
            return None;
        }
        out[i..i + ack.reason.len()].copy_from_slice(ack.reason.as_bytes());
        i += ack.reason.len();
    }
    Some(i)
}
