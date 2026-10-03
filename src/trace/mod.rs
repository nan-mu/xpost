//! Unified [`XdpProg`] + [`Observe`] attach target.
//!
//! [`XDP_PROG`] always exists. With no trace features, [`Observe`] methods are the
//! trait defaults (empty). Features only add storage / fill hooks:
//!
//! - `trace-counter` → [`ext::trace::counter::Counter`](crate::ext::trace::counter::Counter)
//! - `trace-timestamp` → helper ring on the same struct
//!
//! esp-radio owns RX / [`Action`](esp_radio::wifi::Action) mapping / Observe
//! call-sites. This type's [`XdpProg::run`] only performs the leased-image flight.

use xpost_type::{Observe, XdpAction, XdpProg};

#[cfg(feature = "trace-counter")]
use crate::ext::trace::counter::Counter;
#[cfg(feature = "trace-timestamp")]
use crate::ext::trace::timestamp::Timestamp;

/// Process-wide prog singleton (also [`crate::xdp::XDP_PROG`]).
pub struct Trace {
    #[cfg(feature = "trace-counter")]
    counter: Counter,
    #[cfg(feature = "trace-timestamp")]
    timestamp: Timestamp,
}

/// Passed to [`crate::esp_radio::new`] / `wifi::new(..., prog)`; firmware reads here.
pub static XDP_PROG: Trace = Trace {
    #[cfg(feature = "trace-counter")]
    counter: Counter::new(),
    #[cfg(feature = "trace-timestamp")]
    timestamp: Timestamp::new(),
};

/// Soft-fail alias — isolates observation storage from the attach name.
#[cfg(feature = "trace-counter")]
const TRACE: &Trace = &XDP_PROG;

#[cfg(feature = "trace-timestamp")]
pub use crate::ext::trace::timestamp::{Event, EventKind, CAP as TIMESTAMP_CAP};

#[cfg(feature = "trace-counter")]
impl core::ops::Deref for Trace {
    type Target = Counter;

    #[inline]
    fn deref(&self) -> &Counter {
        &self.counter
    }
}

#[cfg(feature = "trace-timestamp")]
impl Trace {
    #[inline]
    pub fn helper_enter(&self, id: usize) {
        self.timestamp.helper_enter(id);
    }

    #[inline]
    pub fn helper_leave(&self, id: usize) {
        self.timestamp.helper_leave(id);
    }

    #[inline]
    pub fn snapshot_timestamps(
        &self,
        out: &mut [crate::ext::trace::timestamp::Event],
    ) -> usize {
        self.timestamp.snapshot_timestamps(out)
    }

    #[inline]
    pub fn clear_timestamps(&self) {
        self.timestamp.clear_timestamps();
    }

    #[inline]
    pub fn timestamp_len(&self) -> usize {
        self.timestamp.timestamp_len()
    }

    #[inline]
    pub fn timestamp_dropped(&self) -> usize {
        self.timestamp.timestamp_dropped()
    }
}

impl XdpProg for Trace {
    /// Lease flight only; driver maps [`XdpAction`] → [`esp_radio::wifi::Action`].
    #[esp_hal::ram]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (XdpAction, &'a mut [u8]) {
        crate::xdp::ingress::run(frame)
    }
}

#[cfg(feature = "trace-counter")]
impl Observe for Trace {
    fn on_drop(&self) {
        self.counter.on_drop();
    }

    fn on_tx_fail_bad_buf(&self) {
        self.counter.on_tx_fail_bad_buf();
    }

    fn on_tx_fail_token(&self) {
        self.counter.on_tx_fail_token();
    }

    fn on_tx_grp_src(&self) {
        self.counter.on_tx_grp_src();
    }

    fn on_tx_enqueued(&self) {
        self.counter.on_tx_enqueued();
    }

    fn on_tx_enq_fail_claim(&self) {
        self.counter.on_tx_enq_fail_claim();
    }

    fn on_tx_enq_fail_ring(&self) {
        self.counter.on_tx_enq_fail_ring();
    }

    fn on_tx_kick(&self) {
        self.counter.on_tx_kick();
    }

    fn on_tx_poll(&self) {
        self.counter.on_tx_poll();
    }

    fn on_tx_cap_wait(&self) {
        self.counter.on_tx_cap_wait();
    }

    fn on_tx_nomem_wait(&self) {
        self.counter.on_tx_nomem_wait();
    }

    fn on_tx_submit_ok(&self) {
        self.counter.on_tx_submit_ok();
    }

    fn on_tx_submit_fail(&self) {
        self.counter.on_tx_submit_fail();
    }

    fn on_tx_no_retain(&self) {
        self.counter.on_tx_no_retain();
    }

    fn on_tx_flush(&self) {
        self.counter.on_tx_flush();
    }

    fn on_tx_air_done(&self) {
        self.counter.on_tx_air_done();
    }

    fn on_tx_air_done_fail(&self) {
        self.counter.on_tx_air_done_fail();
    }

    fn on_tx_netstack_ref(&self) {
        self.counter.on_tx_netstack_ref();
    }

    fn on_tx_netstack_free(&self) {
        self.counter.on_tx_netstack_free();
    }

    fn on_tx_depth_sample(&self) {
        self.counter.on_tx_depth_sample();
    }

    fn on_tx_block_sample(&self) {
        self.counter.on_tx_block_sample();
    }

    fn on_tx_flow_sample(&self) {
        self.counter.on_tx_flow_sample();
    }

    fn on_tx_log_dropped(&self) {
        self.counter.on_tx_log_dropped();
    }
}

#[cfg(not(feature = "trace-counter"))]
impl Observe for Trace {}

/// Soft-fail bumps (no-op without `trace-counter`).
#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_inactive_skip() {
    TRACE.inc_inactive_skip();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_reentry() {
    TRACE.inc_reentry();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_stack_misalign() {
    TRACE.inc_stack_misalign();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_bad_md() {
    TRACE.inc_bad_md();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_helper_unresolved() {
    TRACE.inc_helper_unresolved();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_helper_map() {
    TRACE.inc_helper_map();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_pool_drop_misalign() {
    TRACE.inc_pool_drop_misalign();
}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_inactive_skip() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_reentry() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_stack_misalign() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_bad_md() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_helper_unresolved() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_helper_map() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_pool_drop_misalign() {}
