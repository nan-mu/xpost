//! Default mounted XDP program (lease flight + optional observe storage).
//!
//! The static is `pub(crate)` only — firmware apps read counters via
//! [`crate::ext::trace`], and attach through [`super::esp_radio::new`].

#[cfg(feature = "trace-counter")]
use crate::ext::trace::counter::Counter;
#[cfg(feature = "trace-timestamp")]
use crate::ext::trace::timestamp::Timestamp;

/// Process-wide program singleton type (implements [`xpost_type::XdpProg`]).
///
/// Named after the role, not the trait module path. Trait methods use the
/// fully-qualified [`xpost_type::XdpProg`] path to avoid the name clash.
pub(crate) struct XdpProg {
    #[cfg(feature = "trace-counter")]
    pub(crate) counter: Counter,
    #[cfg(feature = "trace-timestamp")]
    pub(crate) timestamp: Timestamp,
}

/// Mounted by [`super::esp_radio::new`]; not part of the public integrator API.
pub(crate) static XDP_PROG: XdpProg = XdpProg {
    #[cfg(feature = "trace-counter")]
    counter: Counter::new(),
    #[cfg(feature = "trace-timestamp")]
    timestamp: Timestamp::new(),
};

impl xpost_type::XdpProg for XdpProg {
    /// Lease flight only; driver maps [`xpost_type::XdpAction`] → Action.
    #[esp_hal::ram]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (xpost_type::XdpAction, &'a mut [u8]) {
        super::run(frame)
    }
}
