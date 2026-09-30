//! Performance backends under `ext` (replaces former `ext::trace`).
//!
//! | Module | Feature | Role |
//! |--------|---------|------|
//! | [`counter`] | `perf-counter` | monotonic counters + soft-fail |
//! | [`aggregate`] | `perf-aggregate` | latency + stretch histogram (high pps) |
//! | [`gauge`] | `perf-gauge` | current + peak |
//! | [`trace`] | `perf-trace` | **sole timeline**: categorized rings of timestamped events |
//! | [`span`] | `perf-span` | dormant site µs **accumulators** (not rings; unused in profiles) |
//! | [`pchit`] | `perf-pchit` | dormant PC-hit histogram |
//! | [`report`] | any `perf-*` | binary `report` encode / `clear_perf` |
//! | [`esp_radio_trace`] | (internal) | `EspRadioTrace` impl on mounted [`XdpProg`](crate::xdp::ingress::XdpProg) |
//!
//! Timeline model: every flame / path stamp is a ring event (`t_us` + kind).
//! Nest spans are paired `SpanBegin`/`SpanEnd` (or helper enter/leave) on the
//! same ring category — not a separate store. High-pps formal runs use
//! Counter/Aggregate/Gauge without flooding Trace.
//!
//! Storage lives on the crate-mounted program in [`crate::xdp::ingress`]; this
//! module owns [`EspRadioTrace`](xpost_type::EspRadioTrace) impls and the public
//! read face (no `XDP_PROG` export).

#[cfg(feature = "perf-counter")]
pub mod counter;
#[cfg(feature = "perf-aggregate")]
pub mod aggregate;
#[cfg(feature = "perf-gauge")]
pub mod gauge;
#[cfg(feature = "perf-trace")]
pub mod trace;
#[cfg(feature = "perf-span")]
pub mod span;
#[cfg(feature = "perf-pchit")]
pub mod pchit;

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace",
    feature = "perf-span",
    feature = "perf-pchit"
))]
pub mod report;

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

#[cfg(feature = "perf-span")]
#[inline]
pub fn span() -> &'static span::SpanSet {
    &crate::xdp::ingress::XDP_PROG.span
}

#[cfg(feature = "perf-pchit")]
#[inline]
pub fn pchit() -> &'static pchit::PcHitSet {
    &crate::xdp::ingress::XDP_PROG.pchit
}

/// Enable/disable span + PC-hit sampling (`prof on|off`).
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
