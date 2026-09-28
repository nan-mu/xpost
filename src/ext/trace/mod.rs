//! Trace backends under `ext`.
//!
//! | Module | Feature | Role |
//! |--------|---------|------|
//! | [`counter`] | `trace-counter` | atomics; [`Observe`](xpost_type::Observe) on ingress [`XdpProg`](crate::xdp::ingress::XdpProg) |
//! | [`timestamp`] | `trace-timestamp` | helper enter/leave ring |
//!
//! Storage lives on the crate-mounted program in [`crate::xdp::ingress`]; this
//! module owns Observe impls and the public read face (no `XDP_PROG` export).

#[cfg(feature = "trace-counter")]
pub mod counter;
#[cfg(feature = "trace-timestamp")]
pub mod timestamp;

#[cfg(feature = "trace-counter")]
use xpost_type::Observe;

#[cfg(feature = "trace-counter")]
use crate::xdp::ingress::XdpProg;

/// Read-only view of driver / soft-fail counters on the mounted program.
#[cfg(feature = "trace-counter")]
#[inline]
pub fn counter() -> &'static counter::Counter {
    &crate::xdp::ingress::XDP_PROG.counter
}

/// Read-only view of the helper timestamp ring.
#[cfg(feature = "trace-timestamp")]
#[inline]
pub fn timestamp() -> &'static timestamp::Timestamp {
    &crate::xdp::ingress::XDP_PROG.timestamp
}

#[cfg(feature = "trace-timestamp")]
pub use timestamp::{Event, EventKind, CAP as TIMESTAMP_CAP};

#[cfg(feature = "trace-counter")]
impl Observe for XdpProg {
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
