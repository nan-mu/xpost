//! Default mounted XDP program (lease flight + optional perf storage).
//!
//! The static is `pub(crate)` only — firmware apps read via
//! [`crate::ext::perf`], and attach through [`super::esp_radio::new`].

#[cfg(feature = "perf-counter")]
use crate::ext::perf::counter::Counter;
#[cfg(feature = "perf-aggregate")]
use crate::ext::perf::aggregate::AggregateSet;
#[cfg(feature = "perf-gauge")]
use crate::ext::perf::gauge::GaugeSet;
#[cfg(feature = "perf-trace")]
use crate::ext::perf::trace::Trace;
#[cfg(feature = "perf-span")]
use crate::ext::perf::prof::span::SpanSet;
#[cfg(feature = "perf-pchit")]
use crate::ext::perf::prof::pchit::PcHitSet;

/// Process-wide program singleton type (implements [`xpost_type::XdpProg`]).
///
/// Named after the role, not the trait module path. Trait methods use the
/// fully-qualified [`xpost_type::XdpProg`] path to avoid the name clash.
pub(crate) struct XdpProg {
    #[cfg(feature = "perf-counter")]
    pub(crate) counter: Counter,
    #[cfg(feature = "perf-aggregate")]
    pub(crate) aggregate: AggregateSet,
    #[cfg(feature = "perf-gauge")]
    pub(crate) gauge: GaugeSet,
    #[cfg(feature = "perf-trace")]
    pub(crate) trace: Trace,
    #[cfg(feature = "perf-span")]
    pub(crate) span: SpanSet,
    #[cfg(feature = "perf-pchit")]
    pub(crate) pchit: PcHitSet,
}

/// Mounted by [`super::esp_radio::new`]; not part of the public integrator API.
pub(crate) static XDP_PROG: XdpProg = XdpProg {
    #[cfg(feature = "perf-counter")]
    counter: Counter::new(),
    #[cfg(feature = "perf-aggregate")]
    aggregate: AggregateSet::new(),
    #[cfg(feature = "perf-gauge")]
    gauge: GaugeSet::new(),
    #[cfg(feature = "perf-trace")]
    trace: Trace::new(),
    #[cfg(feature = "perf-span")]
    span: SpanSet::new(),
    #[cfg(feature = "perf-pchit")]
    pchit: PcHitSet::new(),
};

impl xpost_type::XdpProg for XdpProg {
    /// Lease flight only; driver maps [`xpost_type::XdpAction`] → Action.
    #[esp_hal::ram]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (xpost_type::XdpAction, &'a mut [u8]) {
        super::run(frame)
    }
}
