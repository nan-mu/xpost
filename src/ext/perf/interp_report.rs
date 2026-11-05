//! Interpreter-arm report in the `xpost-perf` format.
//!
//! Separate from [`super::report`] on purpose. That module encodes a fixed slot
//! layout whose slot names belong to the HAL arm, and the interpreter needs a
//! metric those slots do not have: the engine call alone.
//!
//! The format here carries its own names, so a host labels every entry from the
//! bytes and needs no knowledge of which firmware produced them. Nothing in
//! this file changes the fixed layout, so the published baselines still decode.
//!
//! # What the entries mean
//!
//! - `engine` — the engine call inside one flight, between the `IN_FLIGHT` claim
//!   and its release. This is the number that compares AOT against the
//!   interpreter; it excludes the counter and trace hooks on both sides.
//! - `execution` — the wider interval the perf aggregates already report, from
//!   `xdp_invocations` to `xdp_action`. Kept so the two can be compared on one
//!   wire and the difference between them is visible.
//! - `invocations` — how many flights ran.

use core::convert::Infallible;

use xpost_perf::{Kind, SpanStats, write_entry, write_header};

use crate::ext::perf::aggregate;

/// Longest entry name this report writes.
const MAX_NAME: usize = 16;

/// Names, in wire order.
const NAMES: [&str; 3] = ["engine", "execution", "invocations"];

/// Bytes one entry occupies, given the fixed names above.
const fn entry_bytes(name: &str, kind: Kind) -> usize {
    1 + name.as_bytes().len() + 1 + kind.fixed_len()
}

/// Exact payload length, so the host can frame the blob before reading it.
pub fn payload_len() -> usize {
    xpost_perf::HEADER_LEN
        + entry_bytes(NAMES[0], Kind::Span)
        + entry_bytes(NAMES[1], Kind::Span)
        + entry_bytes(NAMES[2], Kind::Counter)
}

/// Read the aggregates into the report's statistics.
fn stats() -> [SpanStats; 3] {
    let a = aggregate();
    let mut engine = SpanStats::new();
    let mut execution = SpanStats::new();

    let e = a.xdp_engine.snapshot();
    if e.count > 0 {
        engine.count = e.count;
        engine.total_us = e.total_ticks;
        engine.min_us = e.min_ticks;
        engine.max_us = e.max_ticks;
    }
    let x = a.xdp_execution.snapshot();
    if x.count > 0 {
        execution.count = x.count;
        execution.total_us = x.total_ticks;
        execution.min_us = x.min_ticks;
        execution.max_us = x.max_ticks;
    }
    [engine, execution, SpanStats::new()]
}

/// Write the report in `xpost-perf` format.
///
/// The buffer is local and fixed, so this allocates nothing and can run from the
/// control plane without disturbing a flight.
pub fn report(mut write: impl FnMut(&[u8]) -> Result<(), Infallible>) {
    let [engine, execution, _] = stats();
    let invocations = engine.count;

    // Header plus three entries; the longest name bounds the rest.
    const MAX: usize = xpost_perf::HEADER_LEN
        + 3 * (1 + MAX_NAME + 1 + Kind::Span.fixed_len());
    let mut buf = [0u8; MAX];

    let mut o = match write_header(&mut buf, 3) {
        Some(o) => o,
        None => return,
    };
    for (name, kind, st, value) in [
        (NAMES[0], Kind::Span, engine, (0u64, 0u64)),
        (NAMES[1], Kind::Span, execution, (0u64, 0u64)),
        (NAMES[2], Kind::Counter, SpanStats::new(), (invocations, 0u64)),
    ] {
        match write_entry(&mut buf, o, name, kind, &st, value, &[]) {
            Some(next) => o = next,
            None => return,
        }
    }
    let _ = write(&buf[..o]);
}
