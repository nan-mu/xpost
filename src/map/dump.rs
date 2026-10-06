//! ASCII encoder for App-mode `get map` blobs (`OK get map <n>` payload).
//!
//! Layout (UTF-8 text, no trailing blob terminator):
//!
//! ```text
//! array <size>\n
//! <slot> <u64>\n
//! …
//! ```
//!
//! ```text
//! hash <occupancy> <cap>\n
//! <key-token> <value-token>\n
//! …
//! ```
//!
//! ARRAY slots are decimal indices `0..size-1`. HASH keys are dotted IPv4 when
//! the key is 4 bytes (network order); otherwise lowercase hex. HASH values are
//! the little-endian u64 (decimal) when the value is ≤ 8 bytes; otherwise
//! lowercase hex of the full value.

/// Decimal digit count for `n` (at least 1).
pub fn dec_len(mut n: u64) -> usize {
    let mut len = 1usize;
    while n >= 10 {
        n /= 10;
        len += 1;
    }
    len
}

fn write_dec(mut n: u64, write: &mut impl FnMut(&[u8])) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    write(&buf[i..]);
}

/// Format a HASH key as a wire token (dotted IPv4 or hex).
pub fn format_hash_key<'a>(key: &[u8], scratch: &'a mut [u8; 64]) -> &'a [u8] {
    if key.len() == 4 {
        // a.b.c.d — worst case 15 bytes.
        let mut i = 0usize;
        for (idx, b) in key.iter().enumerate() {
            if idx > 0 {
                scratch[i] = b'.';
                i += 1;
            }
            let mut tmp = [0u8; 3];
            let mut n = *b as u64;
            let mut t = 3;
            loop {
                t -= 1;
                tmp[t] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            let digs = &tmp[t..];
            scratch[i..i + digs.len()].copy_from_slice(digs);
            i += digs.len();
        }
        &scratch[..i]
    } else {
        const HEX: &[u8] = b"0123456789abcdef";
        let n = key.len().min(32);
        for (i, b) in key.iter().take(n).enumerate() {
            scratch[i * 2] = HEX[(b >> 4) as usize];
            scratch[i * 2 + 1] = HEX[(b & 0xf) as usize];
        }
        &scratch[..n * 2]
    }
}

/// Interpret map value bytes as a little-endian integer (≤ 8 bytes).
pub fn value_as_u64(value: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = value.len().min(8);
    buf[..n].copy_from_slice(&value[..n]);
    u64::from_le_bytes(buf)
}

/// Header + entries length for an ARRAY snapshot.
pub fn array_blob_len(size: u32, mut value_at: impl FnMut(u32) -> u64) -> usize {
    // "array " + dec(size) + "\n"
    let mut len = 6 + dec_len(u64::from(size)) + 1;
    for i in 0..size {
        let v = value_at(i);
        len += dec_len(u64::from(i)) + 1 + dec_len(v) + 1;
    }
    len
}

/// Write an ARRAY snapshot blob.
pub fn write_array_blob(
    size: u32,
    mut value_at: impl FnMut(u32) -> u64,
    write: &mut impl FnMut(&[u8]),
) {
    write(b"array ");
    write_dec(u64::from(size), write);
    write(b"\n");
    for i in 0..size {
        write_dec(u64::from(i), write);
        write(b" ");
        write_dec(value_at(i), write);
        write(b"\n");
    }
}

/// Length of a HASH header line `hash <size> <cap>\n`.
pub fn hash_header_len(size: u32, cap: u32) -> usize {
    5 + dec_len(u64::from(size)) + 1 + dec_len(u64::from(cap)) + 1
}

/// Write a HASH header line.
pub fn write_hash_header(size: u32, cap: u32, write: &mut impl FnMut(&[u8])) {
    write(b"hash ");
    write_dec(u64::from(size), write);
    write(b" ");
    write_dec(u64::from(cap), write);
    write(b"\n");
}

/// Length of one HASH entry line.
pub fn hash_entry_len(key: &[u8], value: &[u8]) -> usize {
    let mut scratch = [0u8; 64];
    let key_tok = format_hash_key(key, &mut scratch);
    key_tok.len() + 1 + format_hash_value_len(value) + 1
}

/// Write one HASH entry line.
pub fn write_hash_entry(key: &[u8], value: &[u8], write: &mut impl FnMut(&[u8])) {
    let mut scratch = [0u8; 64];
    let key_tok = format_hash_key(key, &mut scratch);
    write(key_tok);
    write(b" ");
    write_hash_value(value, write);
    write(b"\n");
}

fn format_hash_value_len(value: &[u8]) -> usize {
    if value.len() > 8 {
        value.len() * 2
    } else {
        dec_len(value_as_u64(value))
    }
}

fn write_hash_value(value: &[u8], write: &mut impl FnMut(&[u8])) {
    if value.len() > 8 {
        const HEX: &[u8] = b"0123456789abcdef";
        // Emit hex in small chunks to avoid large stack temps.
        let mut chunk = [0u8; 16];
        let mut i = 0usize;
        while i < value.len() {
            let n = (value.len() - i).min(8);
            for (j, b) in value[i..i + n].iter().enumerate() {
                chunk[j * 2] = HEX[(b >> 4) as usize];
                chunk[j * 2 + 1] = HEX[(b & 0xf) as usize];
            }
            write(&chunk[..n * 2]);
            i += n;
        }
    } else {
        write_dec(value_as_u64(value), write);
    }
}
