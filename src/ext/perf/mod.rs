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
//! Nest flame bars are paired ring events (`SpanBegin`/`SpanEnd`), not a fifth
//! exit. Shared plumbing (`clear_perf` / `report_perf`) and dormant site/PC
//! accumulators (`span` / `pchit`) are **crate-private** (`mod`, not `pub mod`):
//! firmware apps and host use only the four accessors + report helpers.
//!
//! [`trace::esp_radio_trace`] lives under `trace/` (EspRadioTrace → all backends)
//! but is wired from this parent so it still builds when only C/A/G are on.

#[cfg(feature = "perf-counter")]
pub mod counter;
#[cfg(feature = "perf-aggregate")]
pub mod aggregate;
#[cfg(feature = "perf-gauge")]
pub mod gauge;
#[cfg(feature = "perf-trace")]
pub mod trace;

#[cfg(feature = "perf-span")]
pub(crate) mod span;
#[cfg(feature = "perf-pchit")]
pub(crate) mod pchit;

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace",
    feature = "perf-span",
    feature = "perf-pchit"
))]
mod report;

/// EspRadioTrace impl — file under `trace/`, linked here for any `perf-*`.
#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace",
    feature = "perf-span",
    feature = "perf-pchit"
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

#[cfg(feature = "perf-span")]
#[inline]
pub(crate) fn span() -> &'static span::SpanSet {
    &crate::xdp::ingress::XDP_PROG.span
}

#[cfg(feature = "perf-pchit")]
#[inline]
pub(crate) fn pchit() -> &'static pchit::PcHitSet {
    &crate::xdp::ingress::XDP_PROG.pchit
}

/// Enable/disable dormant span + PC-hit sampling (`prof on|off` serial).
pub fn set_prof_enabled(on: bool) {
    #[cfg(feature = "perf-span")]
    span::set_enabled(on);
    #[cfg(feature = "perf-pchit")]
    pchit::set_enabled(on);
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
