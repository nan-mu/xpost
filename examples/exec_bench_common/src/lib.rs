//! Shared buffer layout and native HAL compute for `exec_bench_hal`.
//!
//! Used by the HAL App (`run_hal`) and by **host / xpost-test parsing** of map
//! value bytes. The eBPF App carrier does **not** depend on this crate.
//!
//! Layout (must match `examples/exec_bench/ebpf/exec_bench.c`):
//! ```text
//! [0..4]  opcode (LE u32)
//! [4..8]  param  (LE u32)
//! [8..]   payload / scratch
//! ```

#![no_std]

pub const HEADER: usize = 8;
pub const BUF_CAP: usize = 512;
/// Compact ELF / map value size (24-byte payload after header).
pub const MAP_VALUE_LEN: usize = 32;
pub const MAP_PAYLOAD: usize = MAP_VALUE_LEN - HEADER;

pub const OP_INCR: u32 = 0;
pub const OP_SQUARE: u32 = 1;
pub const OP_BITSWAP: u32 = 2;
pub const OP_FIB: u32 = 3;
pub const OP_BUF: u32 = 4;
pub const OP_MEMCPY: u32 = 5;
pub const OP_FLETCHER32: u32 = 6;
pub const OP_BSORT: u32 = 7;

pub const OPCODES: &[(&str, u32)] = &[
    ("incr", OP_INCR),
    ("square", OP_SQUARE),
    ("bitswap", OP_BITSWAP),
    ("fib", OP_FIB),
    ("buf", OP_BUF),
    ("memcpy", OP_MEMCPY),
    ("fletcher32", OP_FLETCHER32),
    ("bsort", OP_BSORT),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BenchError {
    BadOpcode,
    BadBuffer,
    Aborted,
}

#[inline]
pub fn opcode_name(op: u32) -> Option<&'static str> {
    OPCODES.iter().find(|(_, v)| *v == op).map(|(n, _)| *n)
}

#[inline]
pub fn parse_opcode(name: &str) -> Option<u32> {
    let name = name.trim();
    OPCODES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| *v)
}

#[inline]
fn store_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn load_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

/// Default `param` for an opcode (payload sizing / fib n / sort n).
#[inline]
pub fn default_param(op: u32) -> u32 {
    match op {
        OP_INCR | OP_SQUARE | OP_BITSWAP => 0,
        OP_FIB => 30,
        OP_BUF => 64,
        OP_MEMCPY => 64,
        OP_FLETCHER32 => 64,
        OP_BSORT => 16,
        _ => 0,
    }
}

/// Compact-ELF-safe `param` for a 32-byte map value (24-byte payload).
#[inline]
pub fn compact_param(op: u32) -> u32 {
    match op {
        OP_INCR | OP_SQUARE | OP_BITSWAP => 0,
        OP_FIB => 30,
        // buf/fletcher need n+4 bytes; memcpy needs 2n. Cap to MAP_PAYLOAD.
        OP_BUF | OP_FLETCHER32 => 16,
        OP_MEMCPY => 8,
        // n u32s fit in 24 bytes → n ≤ 6.
        OP_BSORT => 6,
        _ => 0,
    }
}

fn fill_payload(payload: &mut [u8], op: u32, param: u32) -> Result<(), BenchError> {
    payload.fill(0);
    match op {
        OP_INCR => store_u32(payload, 0, 0),
        OP_SQUARE => store_u32(payload, 0, 3),
        OP_BITSWAP => store_u32(payload, 0, 0xA5A5_5A5A),
        OP_FIB => store_u32(payload, 0, 0),
        OP_BUF | OP_MEMCPY | OP_FLETCHER32 => {
            let n = param as usize;
            if n > payload.len() {
                return Err(BenchError::BadBuffer);
            }
            for (i, b) in payload.iter_mut().take(n).enumerate() {
                *b = (i.wrapping_mul(17).wrapping_add(3)) as u8;
            }
        }
        OP_BSORT => {
            let n = param as usize;
            if n * 4 > payload.len() {
                return Err(BenchError::BadBuffer);
            }
            for i in 0..n {
                let v = (n as u32 - 1 - i as u32).wrapping_mul(0x9E37_79B9);
                store_u32(payload, i * 4, v);
            }
        }
        _ => return Err(BenchError::BadOpcode),
    }
    Ok(())
}

/// Fill header + deterministic payload for one timed iteration.
pub fn prepare(buf: &mut [u8], op: u32) -> Result<(), BenchError> {
    if buf.len() < BUF_CAP {
        return Err(BenchError::BadBuffer);
    }
    if opcode_name(op).is_none() {
        return Err(BenchError::BadOpcode);
    }
    let param = default_param(op);
    store_u32(buf, 0, op);
    store_u32(buf, 4, param);
    fill_payload(&mut buf[HEADER..], op, param)
}

/// Fill a Compact-ELF 32-byte map value (host / map I/O).
pub fn prepare_map32(buf: &mut [u8], op: u32) -> Result<(), BenchError> {
    if buf.len() < MAP_VALUE_LEN {
        return Err(BenchError::BadBuffer);
    }
    if opcode_name(op).is_none() {
        return Err(BenchError::BadOpcode);
    }
    let param = compact_param(op);
    store_u32(buf, 0, op);
    store_u32(buf, 4, param);
    fill_payload(&mut buf[HEADER..MAP_VALUE_LEN], op, param)?;
    if buf.len() > MAP_VALUE_LEN {
        buf[MAP_VALUE_LEN..].fill(0);
    }
    Ok(())
}

fn bitswap32(mut x: u32) -> u32 {
    x = ((x & 0x5555_5555) << 1) | ((x >> 1) & 0x5555_5555);
    x = ((x & 0x3333_3333) << 2) | ((x >> 2) & 0x3333_3333);
    x = ((x & 0x0f0f_0f0f) << 4) | ((x >> 4) & 0x0f0f_0f0f);
    x = ((x & 0x00ff_00ff) << 8) | ((x >> 8) & 0x00ff_00ff);
    (x << 16) | (x >> 16)
}

fn fib_u32(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    let mut a = 0u32;
    let mut b = 1u32;
    for _ in 1..n {
        let c = a.wrapping_add(b);
        a = b;
        b = c;
    }
    b
}

fn fletcher32(data: &[u8]) -> u32 {
    let mut sum1 = 0xffffu32;
    let mut sum2 = 0xffffu32;
    let mut i = 0usize;
    let mut len = data.len();
    while len > 1 {
        let mut words = len / 2;
        if words > 360 {
            words = 360;
        }
        len -= words * 2;
        while words > 0 {
            let w = u16::from(data[i]) | (u16::from(data[i + 1]) << 8);
            i += 2;
            sum1 += u32::from(w);
            sum2 += sum1;
            words -= 1;
        }
        sum1 = (sum1 & 0xffff) + (sum1 >> 16);
        sum2 = (sum2 & 0xffff) + (sum2 >> 16);
    }
    if len != 0 {
        sum1 += u32::from(data[i]);
        sum2 += sum1;
        sum1 = (sum1 & 0xffff) + (sum1 >> 16);
        sum2 = (sum2 & 0xffff) + (sum2 >> 16);
    }
    sum1 = (sum1 & 0xffff) + (sum1 >> 16);
    sum2 = (sum2 & 0xffff) + (sum2 >> 16);
    (sum2 << 16) | sum1
}

/// One native HAL execution (same semantics as the eBPF image).
pub fn run_hal(buf: &mut [u8]) -> Result<(), BenchError> {
    if buf.len() < HEADER {
        return Err(BenchError::BadBuffer);
    }
    let op = load_u32(buf, 0);
    let param = load_u32(buf, 4);
    let avail = buf.len() - HEADER;
    let payload = &mut buf[HEADER..];

    match op {
        OP_INCR => {
            if avail < 4 {
                return Err(BenchError::Aborted);
            }
            let x = load_u32(payload, 0);
            store_u32(payload, 0, x.wrapping_add(1));
            Ok(())
        }
        OP_SQUARE => {
            if avail < 4 {
                return Err(BenchError::Aborted);
            }
            let x = load_u32(payload, 0);
            store_u32(payload, 0, x.wrapping_mul(x));
            Ok(())
        }
        OP_BITSWAP => {
            if avail < 4 {
                return Err(BenchError::Aborted);
            }
            let x = load_u32(payload, 0);
            store_u32(payload, 0, bitswap32(x));
            Ok(())
        }
        OP_FIB => {
            if avail < 4 {
                return Err(BenchError::Aborted);
            }
            let n = param.min(47);
            store_u32(payload, 0, fib_u32(n));
            Ok(())
        }
        OP_BUF => {
            let n = param as usize;
            if n > avail {
                return Err(BenchError::Aborted);
            }
            let mut acc = 0u32;
            for i in 0..n {
                payload[i] = payload[i].wrapping_add(i as u8);
                acc = acc.wrapping_add(u32::from(payload[i]));
            }
            if avail >= n + 4 {
                store_u32(payload, n, acc);
            }
            Ok(())
        }
        OP_MEMCPY => {
            let n = param as usize;
            if n == 0 || n > avail / 2 {
                return Err(BenchError::Aborted);
            }
            for i in 0..n {
                payload[n + i] = payload[i];
            }
            Ok(())
        }
        OP_FLETCHER32 => {
            let n = param as usize;
            if n == 0 || n > avail || avail < n + 4 {
                return Err(BenchError::Aborted);
            }
            let sum = fletcher32(&payload[..n]);
            store_u32(payload, n, sum);
            Ok(())
        }
        OP_BSORT => {
            let n = param as usize;
            if !(2..=64).contains(&n) || avail < n * 4 {
                return Err(BenchError::Aborted);
            }
            for i in 0..n - 1 {
                for j in 0..n - 1 - i {
                    let a = load_u32(payload, j * 4);
                    let b = load_u32(payload, (j + 1) * 4);
                    if a > b {
                        store_u32(payload, j * 4, b);
                        store_u32(payload, (j + 1) * 4, a);
                    }
                }
            }
            Ok(())
        }
        _ => Err(BenchError::BadOpcode),
    }
}

/// Recompute expected output into `expect` (same prepare seed) and compare to `buf`.
pub fn verify(buf: &[u8]) -> bool {
    if buf.len() < BUF_CAP {
        return false;
    }
    let op = load_u32(buf, 0);
    let mut expect = [0u8; BUF_CAP];
    if prepare(&mut expect, op).is_err() {
        return false;
    }
    if run_hal(&mut expect).is_err() {
        return false;
    }
    // Compare only the region the opcode touches.
    let param = default_param(op) as usize;
    let end = match op {
        OP_INCR | OP_SQUARE | OP_BITSWAP | OP_FIB => HEADER + 4,
        OP_BUF => HEADER + param + 4,
        OP_MEMCPY => HEADER + param * 2,
        OP_FLETCHER32 => HEADER + param + 4,
        OP_BSORT => HEADER + param * 4,
        _ => return false,
    };
    buf[..end] == expect[..end]
}

/// Verify a Compact-ELF 32-byte map value against prepare_map32 + run_hal.
pub fn verify_map32(buf: &[u8]) -> bool {
    if buf.len() < MAP_VALUE_LEN {
        return false;
    }
    let op = load_u32(buf, 0);
    let mut expect = [0u8; MAP_VALUE_LEN];
    if prepare_map32(&mut expect, op).is_err() {
        return false;
    }
    if run_hal(&mut expect).is_err() {
        return false;
    }
    let param = compact_param(op) as usize;
    let end = match op {
        OP_INCR | OP_SQUARE | OP_BITSWAP | OP_FIB => HEADER + 4,
        OP_BUF => HEADER + param + 4,
        OP_MEMCPY => HEADER + param * 2,
        OP_FLETCHER32 => HEADER + param + 4,
        OP_BSORT => HEADER + param * 4,
        _ => return false,
    };
    if end > MAP_VALUE_LEN {
        return false;
    }
    buf[..end] == expect[..end]
}

/// Expected output bytes for `op` after one `prepare_map32` + `run_hal`.
pub fn expected_map32(op: u32) -> Result<[u8; MAP_VALUE_LEN], BenchError> {
    let mut buf = [0u8; MAP_VALUE_LEN];
    prepare_map32(&mut buf, op)?;
    run_hal(&mut buf)?;
    Ok(buf)
}

/// Read RV32 `mcycle` (ESP32-C3).
#[cfg(feature = "mcu")]
#[inline]
pub fn read_mcycle() -> u64 {
    let mut hi: u32;
    let mut lo: u32;
    unsafe {
        core::arch::asm!(
            "1:",
            "rdcycleh {hi}",
            "rdcycle {lo}",
            "rdcycleh {tmp}",
            "bne {hi}, {tmp}, 1b",
            hi = out(reg) hi,
            lo = out(reg) lo,
            tmp = out(reg) _,
            options(nomem, nostack),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Wall-clock microseconds since epoch (same clock as `ext::perf` ticks).
#[cfg(feature = "mcu")]
#[inline]
pub fn now_us() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

/// Time `iters` prepare+run steps; returns `(cycles, us, ok)`.
#[cfg(feature = "mcu")]
pub fn time_loop<F>(buf: &mut [u8], op: u32, iters: u32, mut run_once: F) -> (u64, u64, bool)
where
    F: FnMut(&mut [u8]) -> Result<(), BenchError>,
{
    if prepare(buf, op).is_err() {
        return (0, 0, false);
    }
    // Warm one iteration (not timed).
    let _ = run_once(buf);
    if prepare(buf, op).is_err() {
        return (0, 0, false);
    }

    let t0_us = now_us();
    let t0 = read_mcycle();
    for _ in 0..iters {
        if prepare(buf, op).is_err() {
            return (0, 0, false);
        }
        if run_once(buf).is_err() {
            return (0, 0, false);
        }
    }
    let t1 = read_mcycle();
    let t1_us = now_us();
    let cycles = t1.wrapping_sub(t0);
    let us = t1_us.saturating_sub(t0_us);
    let ok = verify(buf);
    (cycles, us, ok)
}
