//! Performance backends under `ext` (replaces former `ext::trace`).
//!
//! | Module | Feature | Role |
//! |--------|---------|------|
//! | [`counter`] | `perf-counter` | monotonic counters + soft-fail |
//! | [`aggregate`] | `perf-aggregate` | latency + stretch histogram |
//! | [`gauge`] | `perf-gauge` | current + peak |
//! | [`trace`] | `perf-trace` | heapless event ring |
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

mod wire;

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
pub use trace::{Event, EventKind, CAP as TRACE_CAP};

#[cfg(feature = "perf-aggregate")]
pub use aggregate::{AggregateSnapshot, HIST_BUCKETS};
