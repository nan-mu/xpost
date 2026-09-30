//! Performance backends under `ext`.
//!
//! # Public exits (four)
//!
//! | Accessor | Feature | Role |
//! |----------|---------|------|
//! | [`counter`] | `perf-counter` | monotonic counters + soft-fail |
//! | [`aggregate`] | `perf-aggregate` | latency histograms (high pps) |
//! | [`gauge`] | `perf-gauge` | current + peak |
//! | [`trace`] | `perf-trace` | sole timeline — categorized rings of timestamped events |
//!
//! Nest “spans” on the flame are paired ring events (`SpanBegin`/`SpanEnd`),
//! not a fifth exit. Dormant site µs / PC-hit accumulators live under private
//! [`prof`] (`perf-span` / `perf-pchit`, unused in formal profiles).
//!
//! Binary `report` / `perf clear` are shared plumbing ([`clear_perf`],
//! [`report_perf`]), not a metric backend.
//!
//! Storage lives on the crate-mounted program in [`crate::xdp::ingress`]; this
//! module owns the [`EspRadioTrace`](xpost_type::EspRadioTrace) impl and the
//! public read face (no `XDP_PROG` export).

#[cfg(feature = "perf-counter")]
pub mod counter;
#[cfg(feature = "perf-aggregate")]
pub mod aggregate;
#[cfg(feature = "perf-gauge")]
pub mod gauge;
#[cfg(feature = "perf-trace")]
pub mod trace;

#[cfg(any(feature = "perf-span", feature = "perf-pchit"))]
pub(crate) mod prof;

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace",
    feature = "perf-span",
    feature = "perf-pchit"
))]
mod report;

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace",
    feature = "perf-span",
    feature = "perf-pchit"
))]
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

/// Enable/disable dormant span + PC-hit sampling (`prof on|off`).
pub fn set_prof_enabled(on: bool) {
    #[cfg(any(feature = "perf-span", feature = "perf-pchit"))]
    prof::set_enabled(on);
    let _ = on;
}

#[cfg(feature = "perf-trace")]
pub use trace::{Event, EventKind, APP_CAP as TRACE_APP_CAP, CAP as TRACE_CAP, WIRE_LEN as TRACE_WIRE_LEN};

#[cfg(feature = "perf-aggregate")]
pub use aggregate::{AggregateSnapshot, HIST_BUCKETS};

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace",
    feature = "perf-span",
    feature = "perf-pchit"
))]
pub use report::{clear_perf, report_payload_len, report_perf};
