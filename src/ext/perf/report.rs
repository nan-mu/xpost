//! Compile-time feature-gated binary perf report (`report` serial command).
//!
//! Sections in order **C → A → G → T** (omit absent features).
//! Payload length uniquely identifies the feature mask (see host length table).

use core::convert::Infallible;

#[cfg(feature = "perf-counter")]
pub const COUNTER_WIRE_LEN: usize = 184;
#[cfg(feature = "perf-aggregate")]
pub const AGGREGATE_WIRE_LEN: usize = 8 * 176;
#[cfg(feature = "perf-gauge")]
pub const GAUGE_WIRE_LEN: usize = 32;
#[cfg(feature = "perf-trace")]
pub const TRACE_WIRE_LEN: usize = crate::ext::perf::trace::WIRE_LEN;

/// Total payload bytes for the features compiled into this firmware image.
pub const fn report_payload_len() -> usize {
    let mut n = 0usize;
    #[cfg(feature = "perf-counter")]
    {
        n += COUNTER_WIRE_LEN;
    }
    #[cfg(feature = "perf-aggregate")]
    {
        n += AGGREGATE_WIRE_LEN;
    }
    #[cfg(feature = "perf-gauge")]
    {
        n += GAUGE_WIRE_LEN;
    }
    #[cfg(feature = "perf-trace")]
    {
        n += TRACE_WIRE_LEN;
    }
    n
}

/// Reset every compiled-in perf backend.
pub fn clear_perf() {
    #[cfg(feature = "perf-counter")]
    crate::ext::perf::counter().reset_all();
    #[cfg(feature = "perf-aggregate")]
    crate::ext::perf::aggregate().reset_all();
    #[cfg(feature = "perf-gauge")]
    crate::ext::perf::gauge().reset_all();
    #[cfg(feature = "perf-trace")]
    crate::ext::perf::trace().clear();
}

/// Encode all enabled sections into `write` (raw LE bytes, no framing).
pub fn report_perf(mut write: impl FnMut(&[u8]) -> Result<(), Infallible>) {
    let _ = write;
    #[cfg(feature = "perf-counter")]
    {
        let mut buf = [0u8; COUNTER_WIRE_LEN];
        encode_counter(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-aggregate")]
    {
        let mut buf = [0u8; AGGREGATE_WIRE_LEN];
        encode_aggregate(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-gauge")]
    {
        let mut buf = [0u8; GAUGE_WIRE_LEN];
        encode_gauge(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-trace")]
    {
        let mut buf = [0u8; TRACE_WIRE_LEN];
        encode_trace(&mut buf);
        let _ = write(&buf);
    }
}
