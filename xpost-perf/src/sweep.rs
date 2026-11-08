//! Shared scale-sweep harness for the three engines.
//!
//! One loop count, one run count, one output line, so a reader compares engines
//! on identical work. Each firmware supplies only its engine call and its
//! clock; everything else lives here.
//!
//! # Why powers of two
//!
//! A geometric sweep makes a power law visible as a straight line on a log-log
//! plot, and it spreads the points evenly in orders of magnitude. A linear sweep
//! spends most of its points near zero, where the fixed cost dominates and the
//! slope is unreadable.
//!
//! # Why many runs
//!
//! The microsecond clock reads in 0.7 µs and quantizes to whole microseconds.
//! Timing `n` runs together and dividing makes the quantization error `1/n` of a
//! microsecond. At two hundred runs one microsecond of uncertainty becomes five
//! nanoseconds.
//!
//! # What the output must support
//!
//! A regression of time against loop count. The slope is the cost of one loop
//! pass, the intercept is the fixed cost of one call, and the residual says
//! whether a line describes the data at all.

/// Work sizes for a sweep, as a union over the workloads with a wide range.
///
/// Superseded by [`params_for`], which gives each workload its own range. Kept
/// because earlier captures used it.
pub const PARAMS: [u32; 16] = [
    1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47, 64, 128, 256, 512, 1024,
];

/// Sizes to sweep for a workload, from its opcode.
///
/// Each list stays inside the range the workload's own guard accepts. Two are
/// wide (`buf`, `fib`), three are narrow because the source clamps hard
/// (`memcpy` 12, `fletcher32` 20, `bsort` 6), and three do fixed work
/// (`incr`, `square`, `bitswap`).
///
/// The fixed-work three still sweep: their work does not change, so a flat line
/// is the correct result and it measures the call's fixed cost directly.
pub fn params_for(opcode: u32) -> Option<&'static [u32]> {
    const BUF: [u32; 16] = [
        1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47, 64, 128, 256, 512, 1024,
    ];
    const FIB: [u32; 11] = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 47];
    const MEMCPY: [u32; 6] = [1, 2, 4, 6, 9, 12];
    const FLETCHER32: [u32; 7] = [1, 2, 4, 8, 12, 16, 20];
    const BSORT: [u32; 5] = [2, 3, 4, 5, 6];
    // Fixed work: repeat the same size. The figure is flat by construction.
    const FIXED: [u32; 4] = [1, 2, 4, 8];

    match opcode {
        OP_INCR => Some(&FIXED),
        OP_SQUARE => Some(&FIXED),
        OP_BITSWAP => Some(&FIXED),
        OP_FIB => Some(&FIB),
        OP_BUF => Some(&BUF),
        OP_MEMCPY => Some(&MEMCPY),
        OP_FLETCHER32 => Some(&FLETCHER32),
        OP_BSORT => Some(&BSORT),
        _ => None,
    }
}

/// Runs per point.
pub const RUNS: u32 = 200;

/// Bytes a frame needs for a given workload and work size.
///
/// The header is 8 bytes, then the payload the workload reads, then room for the
/// result the workload may write. Two workloads need a floor:
/// `memcpy` and `fletcher32` both check `payload + 24 > end` before they look at
/// `param`, so a frame shorter than 32 bytes is refused regardless of the work
/// size. Giving them a large frame and a small `param` measures the work they
/// actually do.
pub const fn frame_len_for(opcode: u32, param: u32) -> usize {
    let payload = payload_bytes(opcode, param);
    let floor = match opcode {
        OP_MEMCPY | OP_FLETCHER32 => 24,
        // `bsort` also checks `payload + 24 > end` before it looks at `n`, so a
        // two-word array still needs a 24-byte payload region.
        OP_BSORT => 24,
        _ => 0,
    };
    let payload = if payload < floor { floor } else { payload };
    8 + payload + 4
}

/// Bytes a frame needs for a byte-range workload.
pub const fn frame_len(param: u32) -> usize {
    8 + param as usize + 4
}

/// The value size the exec path carries: 32 bytes, header plus a 24-byte
/// payload.
pub const VALUE_LEN: usize = 32;

/// One workload's fixed input, as the 32-byte map value the exec path uses.
///
/// These are the fixtures the exam records, so a measurement here describes the
/// same work the exam's `OK` check accepts. Header: `opcode` at offset 0, `param`
/// at offset 4, payload from offset 8.
///
/// Only `buf` takes a range of loop counts. The others clamp their parameter
/// (`memcpy` to 12, `fletcher32` to 20, `bsort` to 6) or ignore it entirely
/// (`incr`, `square`, `bitswap`), so a scale sweep is not meaningful for them.
/// Measuring them at one size still says something: the fixed cost of a call is
/// what a small workload pays.
pub struct Fixture {
    pub name: &'static str,
    pub value: [u8; VALUE_LEN],
}

const fn fixture(opcode: u32, param: u32, payload: &[u8]) -> [u8; VALUE_LEN] {
    let mut v = [0u8; VALUE_LEN];
    v[0] = opcode as u8;
    v[4] = param as u8;
    let mut i = 0;
    while i < payload.len() && 8 + i < VALUE_LEN {
        v[8 + i] = payload[i];
        i += 1;
    }
    v
}

/// The eight workloads at the size the exam uses.
pub const FIXTURES: [Fixture; 8] = [
    Fixture {
        name: "incr",
        value: fixture(0, 0, &[]),
    },
    Fixture {
        name: "square",
        value: fixture(1, 3, &[3, 0, 0, 0]),
    },
    Fixture {
        name: "bitswap",
        value: fixture(2, 0, &[0x5a, 0xa5, 0xa5, 0x5a]),
    },
    Fixture {
        name: "fib",
        value: fixture(3, 30, &[]),
    },
    Fixture {
        name: "buf",
        value: fixture(4, 16, &[3, 0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7a]),
    },
    Fixture {
        name: "memcpy",
        value: fixture(5, 8, &[3, 0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7a]),
    },
    Fixture {
        name: "fletcher32",
        value: fixture(6, 16, &[3, 0x14, 0x25, 0x36, 0x47, 0x58, 0x69, 0x7a]),
    },
    Fixture {
        name: "bsort",
        value: fixture(7, 6, &[0x9d, 0x60, 0x15, 0x17, 0xe4, 0xe6, 0xdd, 0x78]),
    },
];

/// Workload opcodes, as the exec value's first field.
///
/// The same numbering the App path uses, so a frame means the same thing to
/// every engine.
pub const OP_INCR: u32 = 0;
pub const OP_SQUARE: u32 = 1;
pub const OP_BITSWAP: u32 = 2;
pub const OP_FIB: u32 = 3;
pub const OP_BUF: u32 = 4;
pub const OP_MEMCPY: u32 = 5;
pub const OP_FLETCHER32: u32 = 6;
pub const OP_BSORT: u32 = 7;

/// Fill a frame for a workload whose work size is `param`.
///
/// Header: opcode at offset 0, work size at offset 4, payload from offset 8.
///
/// The payload is seeded according to what the workload reads, because the same
/// `param` means different amounts of data:
///
/// - `bsort` reads `param` **u32 words**, so it needs `param * 4` seeded bytes.
///   Seeding only `param` bytes leaves zeros, the array is already sorted, and
///   the sort does no swaps; the timing then says nothing about the sort.
/// - `fib`, `incr`, `square` and `bitswap` read one u32 and ignore the rest.
/// - `buf`, `memcpy` and `fletcher32` walk `param` bytes.
///
/// The values are chosen so a comparison is not already ordered and a checksum
/// has varied input.
pub fn fill_op(frame: &mut [u8], opcode: u32, param: u32) {
    frame[0..4].copy_from_slice(&opcode.to_le_bytes());
    frame[4..8].copy_from_slice(&param.to_le_bytes());
    // Seed enough to satisfy the workload's own bounds check, not just its work
    // size: `memcpy` and `fletcher32` refuse a frame whose payload is under 24
    // bytes before they look at `param`.
    let bytes = match frame_len_for(opcode, param).checked_sub(12) {
        Some(n) => n,
        None => payload_bytes(opcode, param),
    };
    for i in 0..bytes {
        if 8 + i < frame.len() {
            frame[8 + i] = 0x10u8.wrapping_add((i as u8).wrapping_mul(7));
        }
    }
    // Clear whatever follows the payload so a run cannot read a previous size's
    // leftover bytes.
    let tail = 8 + bytes;
    let end = tail.saturating_add(4).min(frame.len());
    for b in frame[tail..end].iter_mut() {
        *b = 0;
    }
}

/// Seeded payload bytes for a workload at a given work size.
pub const fn payload_bytes(opcode: u32, param: u32) -> usize {
    match opcode {
        // A u32 array of `param` words.
        OP_BSORT => param as usize * 4,
        // One u32 of input; the rest is unused.
        OP_INCR | OP_SQUARE | OP_BITSWAP | OP_FIB => 4,
        // A byte range of `param` bytes.
        _ => param as usize,
    }
}

/// Fill a frame for the `buf` workload.
pub fn fill_buf(frame: &mut [u8], param: u32) {
    fill_op(frame, OP_BUF, param);
}
