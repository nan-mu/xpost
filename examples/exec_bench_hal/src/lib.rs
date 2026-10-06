#![no_std]

//! Native HAL twin of `exec_bench` (no `xpost` runtime).
//!
//! Re-export [`hal_perf`] for CAG report + App serial map wire helpers.
//! Workload bytes are interpreted here, not in `xpost-serial`.

pub use hal_perf as cag;
pub use hal_perf::wire;

const HEADER: usize = 8;
const OP_INCR: u32 = 0;
const OP_SQUARE: u32 = 1;
const OP_BITSWAP: u32 = 2;
const OP_FIB: u32 = 3;
const OP_BUF: u32 = 4;
const OP_MEMCPY: u32 = 5;
const OP_FLETCHER32: u32 = 6;
const OP_BSORT: u32 = 7;

fn store_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

fn load_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
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

/// Fletcher-32 as in Wikipedia / RFC 3720 §12.1 (360-word inner chunks).
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

/// One native execution on the 32-byte map value (same layout as the XDP images).
pub fn run_hal(buf: &mut [u8]) -> Result<(), ()> {
    if buf.len() < HEADER {
        return Err(());
    }
    let op = load_u32(buf, 0);
    let param = load_u32(buf, 4);
    let avail = buf.len() - HEADER;
    let payload = &mut buf[HEADER..];

    match op {
        OP_INCR => {
            if avail < 4 {
                return Err(());
            }
            let x = load_u32(payload, 0);
            store_u32(payload, 0, x.wrapping_add(1));
            Ok(())
        }
        OP_SQUARE => {
            if avail < 4 {
                return Err(());
            }
            let x = load_u32(payload, 0);
            store_u32(payload, 0, x.wrapping_mul(x));
            Ok(())
        }
        OP_BITSWAP => {
            if avail < 4 {
                return Err(());
            }
            let x = load_u32(payload, 0);
            store_u32(payload, 0, x.reverse_bits());
            Ok(())
        }
        OP_FIB => {
            if avail < 4 {
                return Err(());
            }
            store_u32(payload, 0, fib_u32(param.min(47)));
            Ok(())
        }
        OP_BUF => {
            let n = param as usize;
            if n > avail {
                return Err(());
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
                return Err(());
            }
            payload.copy_within(0..n, n);
            Ok(())
        }
        OP_FLETCHER32 => {
            let n = param as usize;
            if n == 0 || n > avail || avail < n + 4 {
                return Err(());
            }
            let sum = fletcher32(&payload[..n]);
            store_u32(payload, n, sum);
            Ok(())
        }
        OP_BSORT => {
            let n = param as usize;
            if !(2..=64).contains(&n) || avail < n * 4 {
                return Err(());
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
        _ => Err(()),
    }
}
