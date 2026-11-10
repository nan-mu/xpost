//! Report contents declared per firmware, not per Cargo feature.
//!
//! The fixed-slot report shipped every compiled-in backend in a fixed order, so
//! a firmware could not say "this experiment reports these eight numbers and
//! nothing else". This module reverses that: each firmware declares a `&[MetricSpec]`,
//! and the encoder writes exactly those entries.
//!
//! # Why the getters are function pointers
//!
//! This crate has no dependencies and no idea where a counter lives. Each
//! firmware supplies a getter per metric that reads its own storage. The getter
//! set is a plain slice of `Copy` descriptors, so it can be a `const`, which
//! keeps the declaration next to the code that fills the same counters.
//!
//! # Why one encoder for measuring and writing
//!
//! `get perf` announces the payload length before any payload byte goes out. That
//! length has to come from the same plan the body is written from, so
//! [`report_len`] and [`encode_report`] walk the same descriptors. A separate
//! length function is a second place for the two to disagree, and a mismatch
//! cannot be repaired after the header is on the wire.

use crate::{
    encode_events, encode_header, encode_metric, entry_len, event_entry_len, Event, Kind, Sink,
    SpanStats,
};

/// Longest histogram this crate describes.
///
/// The wire format allows any edge count. Firmware histograms have this many
/// buckets, and bounding it keeps [`MetricValue`] a fixed size so a getter can
/// return it with no allocator.
pub const MAX_BUCKETS: usize = 16;

/// One metric's value, in whatever shape its kind needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricValue {
    /// A monotonically increasing tally.
    Counter(u64),
    /// A duration measured many times.
    Span(SpanStats),
    /// A current value with a high-water mark.
    Gauge { current: u64, peak: u64 },
    /// A duration with fixed bucket edges. `buckets` holds one count per edge.
    Histogram {
        stats: SpanStats,
        edges: &'static [u64],
        buckets: [u64; MAX_BUCKETS],
    },
}

impl MetricValue {
    /// The kind this value writes as.
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Counter(_) => Kind::Counter,
            Self::Span(_) => Kind::Span,
            Self::Gauge { .. } => Kind::Gauge,
            Self::Histogram { .. } => Kind::Histogram,
        }
    }

    /// Histogram edges, or an empty slice for other kinds.
    const fn edges(&self) -> &'static [u64] {
        match self {
            Self::Histogram { edges, .. } => edges,
            _ => &[],
        }
    }
}

/// One metric a firmware reports.
///
/// `name` is what the host stores, so it carries the unit suffix.
#[derive(Clone, Copy)]
pub struct MetricSpec {
    pub name: &'static str,
    pub get: fn() -> MetricValue,
}

impl core::fmt::Debug for MetricSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MetricSpec")
            .field("name", &self.name)
            .finish()
    }
}

/// One event ring a firmware reports.
///
/// The getter writes as many events as `out` holds and returns the ring's
/// overflow count plus the total number of events it had. It must report the
/// true total even when `out` is shorter, because [`report_len`] measures with an
/// empty slice.
#[derive(Clone, Copy)]
pub struct EventSpec {
    pub name: &'static str,
    pub get: fn(out: &mut [Event]) -> (u64, usize),
}

impl core::fmt::Debug for EventSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EventSpec")
            .field("name", &self.name)
            .finish()
    }
}

fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= crate::MAX_NAME && name.is_ascii()
}

/// Payload bytes for this plan, or `None` when the plan cannot be encoded.
///
/// Checks names and histogram shapes, so a caller learns about a bad plan here
/// instead of from a half-written frame.
pub fn report_len(metrics: &[MetricSpec], events: Option<EventSpec>) -> Option<usize> {
    let mut n = crate::HEADER_LEN;
    for spec in metrics {
        if !name_ok(spec.name) {
            return None;
        }
        let value = (spec.get)();
        let tail = if value.kind() == Kind::Histogram {
            let edges = value.edges();
            // An empty edge list describes no bin at all, so refuse it here
            // rather than publish a histogram with no buckets.
            if edges.is_empty() || edges.len() > MAX_BUCKETS {
                return None;
            }
            // Each bucket is an edge and a count, so 16 bytes, not 8.
            edges.len() * 16
        } else {
            0
        };
        n += entry_len(spec.name, value.kind(), tail);
    }
    if let Some(ev) = events {
        if !name_ok(ev.name) {
            return None;
        }
        let mut none: [Event; 0] = [];
        let (_, count) = (ev.get)(&mut none);
        n += event_entry_len(ev.name, count);
    }
    Some(n)
}

/// Entries in this plan, for the header's count field.
pub fn entry_count(metrics: &[MetricSpec], events: Option<EventSpec>) -> u16 {
    metrics.len() as u16 + u16::from(events.is_some())
}

/// Writes the whole report into `sink`. Returns `false` on a bad name or
/// histogram shape, or when the sink refuses a write.
///
/// Entry order is the plan's order, so the same plan yields the same bytes.
/// `events` is the caller's snapshot buffer. It must be at least as long as the
/// ring, because a truncated snapshot would look like a shorter timeline.
pub fn encode_report(
    metrics: &[MetricSpec],
    events: Option<EventSpec>,
    event_buf: &mut [Event],
    sink: &mut impl Sink,
) -> bool {
    if !encode_header(sink, entry_count(metrics, events)) {
        return false;
    }
    for spec in metrics {
        if !name_ok(spec.name) {
            return false;
        }
        let value = (spec.get)();
        match value {
            MetricValue::Counter(v) => {
                if !encode_metric(sink, spec.name, Kind::Counter, &SpanStats::new(), (v, 0), &[], &[]) {
                    return false;
                }
            }
            MetricValue::Span(stats) => {
                if !encode_metric(sink, spec.name, Kind::Span, &stats, (0, 0), &[], &[]) {
                    return false;
                }
            }
            MetricValue::Gauge { current, peak } => {
                if !encode_metric(
                    sink,
                    spec.name,
                    Kind::Gauge,
                    &SpanStats::new(),
                    (current, peak),
                    &[],
                    &[],
                ) {
                    return false;
                }
            }
            MetricValue::Histogram { stats, edges, buckets } => {
                if edges.is_empty() || edges.len() > MAX_BUCKETS {
                    return false;
                }
                if !encode_metric(
                    sink,
                    spec.name,
                    Kind::Histogram,
                    &stats,
                    (0, 0),
                    edges,
                    &buckets[..edges.len()],
                ) {
                    return false;
                }
            }
        }
    }
    if let Some(ev) = events {
        let (dropped, count) = (ev.get)(event_buf);
        // A short buffer would publish a shortened timeline with no marker, so
        // refuse instead.
        if count > event_buf.len() {
            return false;
        }
        if !encode_events(sink, ev.name, dropped, &event_buf[..count]) {
            return false;
        }
    }
    true
}
