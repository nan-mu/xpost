//! Performance backends under `ext`.
//!
//! # Public exits (four)
//!
//! | Accessor | Feature | Role |
//! |----------|---------|------|
//! | [`counter`] | `perf-counter` | monotonic counters |
//! | [`aggregate`] | `perf-aggregate` | latency histograms (high pps) |
//! | [`gauge`] | `perf-gauge` | current + peak |
//! | [`trace`] | `perf-trace` | sole timeline — categorized rings |
//!
//! Nest flame bars are paired ring events (`SpanBegin`/`SpanEnd`), not a
//! separate accumulator backend. Shared plumbing: [`clear_perf`] / [`report_perf`].

#[cfg(feature = "perf-counter")]
pub mod counter;
#[cfg(feature = "perf-aggregate")]
pub mod aggregate;
#[cfg(feature = "perf-gauge")]
pub mod gauge;
#[cfg(feature = "perf-trace")]
pub mod trace;

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace"
))]
mod report;

/// EspRadioTrace impl — file under `trace/`, linked here for any `perf-*`.
#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace"
))]
#[path = "trace/esp_radio_trace.rs"]
mod esp_radio_trace;

#[cfg(feature = "perf-counter")]
#[inline]
pub fn counter() -> &'static counter::Counter {
    &crate::xdp::ingress::XDP_PROG.counter
}

#[cfg(feature = "perf-aggregate")]
#[inline]
pub fn aggregate() -> &'static aggregate::AggregateSet {
    &crate::xdp::ingress::XDP_PROG.aggregate
}

#[cfg(feature = "perf-gauge")]
#[inline]
pub fn gauge() -> &'static gauge::GaugeSet {
    &crate::xdp::ingress::XDP_PROG.gauge
}

#[cfg(feature = "perf-trace")]
#[inline]
pub fn trace() -> &'static trace::Trace {
    &crate::xdp::ingress::XDP_PROG.trace
}

#[cfg(feature = "perf-trace")]
pub use trace::{Event, EventKind, APP_CAP as TRACE_APP_CAP, CAP as TRACE_CAP, WIRE_LEN as TRACE_WIRE_LEN};

#[cfg(feature = "perf-aggregate")]
pub use aggregate::{AggregateSnapshot, HIST_BUCKETS};

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace"
))]
pub use report::{clear_perf, report_payload_len, report_perf};
