//! The firmware report: which entries this build sends, and how they are read.
//!
//! Every entry here is declared once, in [`METRICS`]. The encoder writes exactly
//! that list, so a reader labels the bytes from the plan and needs no per-build
//! knowledge. The old fixed-slot layout is gone: it named its slots for one arm
//! and had no room for a number another arm needed.
//!
//! # Why these three
//!
//! - `engine_us` is the call itself: from the `IN_FLIGHT` claim to its release.
//!   It is the only figure both engines measure identically, so it is the one to
//!   compare them with.
//! - `execution_us` is the wider interval between `xdp_invocations` and
//!   `xdp_action`. It includes the counter and trace hooks on both sides, so it
//!   describes the accounting as much as the work. Kept so the gap between the
//!   two is visible.
//! - `invocations` is how many flights ran. A comparison between a figure from
//!   one flight and a figure from none is not a comparison.
//!
//! Both counters are already recorded inside `ingress::run` for either engine, so
//! a build without the interpreter still reports the same names.

use core::convert::Infallible;

use xpost_perf::plan::{encode_report, entry_count, report_len, EventSpec, MetricSpec, MetricValue};
use xpost_perf::{Kind, Sink, SpanStats};

use crate::ext::perf::aggregate;

fn engine() -> MetricValue {
    MetricValue::Span(span_of(&aggregate().xdp_engine.snapshot()))
}

fn execution() -> MetricValue {
    MetricValue::Span(span_of(&aggregate().xdp_execution.snapshot()))
}

fn invocations() -> MetricValue {
    MetricValue::Counter(aggregate().xdp_engine.snapshot().count)
}

fn span_of(snap: &aggregate::AggregateSnapshot) -> SpanStats {
    // Ticks, not microseconds: the system timer resolves 62.5 ns and a
    // microsecond clock would round a one-microsecond call to 1 or 2 with no
    // information in between.
    if snap.count == 0 {
        return SpanStats::new();
    }
    SpanStats {
        count: snap.count,
        total_us: snap.total_ticks,
        min_us: snap.min_ticks,
        max_us: snap.max_ticks,
    }
}

/// Names, in wire order. The unit is in the name so a reader cannot mistake
/// ticks for microseconds, which is what made the earlier numbers look like they
/// described the engine.
pub const METRICS: &[MetricSpec] = &[
    MetricSpec {
        name: "engine_us",
        get: engine,
    },
    MetricSpec {
        name: "execution_us",
        get: execution,
    },
    MetricSpec {
        name: "invocations",
        get: invocations,
    },
];

/// The flight's timeline.
///
/// A single-run experiment needs the event order as well as the totals: the
/// totals say how long the call took, and the events say what happened inside
/// it. The ring is drained into the caller's buffer, so this reports what the
/// live ring held rather than a second copy.
fn events(out: &mut [xpost_perf::Event]) -> (u64, usize) {
    let ring = crate::ext::perf::trace();
    let mut radio = [crate::ext::perf::Event::default(); crate::ext::perf::TRACE_CAP];
    let n = ring.snapshot_radio(&mut radio);
    let take = n.min(out.len());
    for (slot, ev) in out.iter_mut().zip(radio.iter()).take(take) {
        *slot = xpost_perf::Event {
            kind: ev.kind,
            arg0: ev.arg0,
            arg1: ev.arg1,
            t_us: ev.t_us,
        };
    }
    (ring.dropped() as u64, n)
}

/// The one event stream this report carries.
pub const FLIGHT: EventSpec = EventSpec {
    name: "flight",
    get: events,
};

/// Payload length, so the host can frame the blob before reading it.
pub fn payload_len() -> usize {
    report_len(METRICS, Some(FLIGHT)).unwrap_or(0)
}

/// Entries this report sends.
pub fn entries() -> u16 {
    entry_count(METRICS, Some(FLIGHT))
}

/// Stream the report through `sink`.
///
/// Writes straight into the serial writer: the report needs no buffer of its
/// own, and the length it announced came from the same plan.
pub fn report(sink: &mut WriteSink<'_>) -> bool {
    // A fixed snapshot buffer, because the report crate has no allocator and the
    // ring's capacity is a compile-time constant. A full ring is the worst case,
    // so the buffer cannot be short.
    let mut events = [xpost_perf::Event::default(); crate::ext::perf::TRACE_CAP];
    encode_report(METRICS, Some(FLIGHT), &mut events, sink)
}

/// Adapts the serial reply writer to a [`Sink`].
///
/// Holds a trait object because `ReplyWriter::get_perf` hands the encoder a
/// `&mut dyn FnMut`, and a generic wrapper would not accept that unsized type.
pub struct WriteSink<'a> {
    write: &'a mut dyn FnMut(&[u8]) -> Result<(), Infallible>,
}

impl<'a> WriteSink<'a> {
    pub fn new(write: &'a mut dyn FnMut(&[u8]) -> Result<(), Infallible>) -> Self {
        Self { write }
    }
}

impl Sink for WriteSink<'_> {
    fn put(&mut self, bytes: &[u8]) -> bool {
        (self.write)(bytes).is_ok()
    }
}

/// Names this report declares, for a host that wants to check it saw them all.
pub fn names() -> impl Iterator<Item = &'static str> {
    METRICS.iter().map(|m| m.name)
}

/// Kind of the entry at `i`, or `None` when out of range.
pub fn kind_at(i: usize) -> Option<Kind> {
    METRICS.get(i).map(|m| (m.get)().kind())
}
