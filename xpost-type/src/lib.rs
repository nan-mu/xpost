//! Shared XDP program / observe types for esp-radio ↔ xpost.
//!
//! `no_std`, no chip deps. Enable `defmt` for [`XdpAction`] formatting.
//!
//! Hierarchy: [`Observe`]: [`XdpProg`] — observation extends a program.
//! A bare [`XdpProg`] need not implement [`Observe`]. Storage types (counters)
//! are not [`Observe`]; only the mounted prog is.

#![no_std]

use core::fmt;

/// Linux `enum xdp_action` as returned by [`XdpProg::run`].
///
/// `XDP_REDIRECT` (4) is folded to [`XdpAction::Tx`] (no devmap) when converting
/// from raw `i32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum XdpAction {
    /// `XDP_ABORTED` — treat as drop at the driver.
    Aborted = 0,
    /// `XDP_DROP`.
    Drop = 1,
    /// `XDP_PASS` — deliver window to the stack.
    Pass = 2,
    /// `XDP_TX` — same-iface zero-copy transmit.
    Tx = 3,
}

impl From<i32> for XdpAction {
    #[inline]
    fn from(v: i32) -> Self {
        match v {
            0 => Self::Aborted,
            1 => Self::Drop,
            2 => Self::Pass,
            3 | 4 => Self::Tx,
            _ => Self::Aborted,
        }
    }
}

impl fmt::Display for XdpAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Aborted => "XDP_ABORTED",
            Self::Drop => "XDP_DROP",
            Self::Pass => "XDP_PASS",
            Self::Tx => "XDP_TX",
        })
    }
}

/// Program mounted on the Wi-Fi RX ingress path.
///
/// `run` returns Linux-style [`XdpAction`] plus a visible window (subslice of
/// `frame`). The driver maps that to Pass / Drop / Tx.
pub trait XdpProg: Send + Sync {
    /// Inspect/modify `frame` and return an [`XdpAction`] plus the visible window.
    fn run<'a>(&self, frame: &'a mut [u8]) -> (XdpAction, &'a mut [u8]);
}

/// Observation hooks layered on a mounted [`XdpProg`].
///
/// The driver only **calls** these (when `xdp-observe` is enabled); counting
/// and timestamps are owned by the implementor. Counter-like storage is not
/// an [`Observe`] — only the program is.
pub trait Observe: XdpProg {
    /// RX dispatch enter, before [`XdpProg::run`].
    fn on_rx_enter(&self) {}
    /// About to call [`XdpProg::run`].
    fn on_xdp_enter(&self) {}
    /// After [`XdpProg::run`] returns (any [`XdpAction`]).
    fn on_xdp_leave(&self) {}
    /// Pass queued, about to wake stack.
    fn on_pass_to_stack(&self) {}
    /// Drop after free_rx_buffer.
    fn on_drop(&self) {}
    /// Tx transmit → FailBadBuf.
    fn on_tx_fail_bad_buf(&self) {}
    /// Tx transmit → FailToken / FailRing.
    fn on_tx_fail_token(&self) {}

    /// Group source MAC rejected.
    fn on_tx_grp_src(&self) {}
    /// Ring enqueue success.
    fn on_tx_enqueued(&self) {}
    /// Token/claim failure.
    fn on_tx_enq_fail_claim(&self) {}
    /// Ring full.
    fn on_tx_enq_fail_ring(&self) {}
    /// Kick TX task after enqueue.
    fn on_tx_kick(&self) {}
    /// Poll entry.
    fn on_tx_poll(&self) {}
    /// Stopped on inflight cap.
    fn on_tx_cap_wait(&self) {}
    /// NOMEM wait.
    fn on_tx_nomem_wait(&self) {}
    /// `tx_by_ref` OK (≠ air done).
    fn on_tx_submit_ok(&self) {}
    /// `tx_by_ref` failed.
    fn on_tx_submit_fail(&self) {}
    /// Handoff without driver retain.
    fn on_tx_no_retain(&self) {}
    /// Pending ring flushed.
    fn on_tx_flush(&self) {}
    /// Air TX success.
    fn on_tx_air_done(&self) {}
    /// Air TX failure.
    fn on_tx_air_done_fail(&self) {}
    /// netstack_ref callback.
    fn on_tx_netstack_ref(&self) {}
    /// netstack_free callback.
    fn on_tx_netstack_free(&self) {}
    /// Depth sample after enqueue/drain.
    fn on_tx_depth_sample(&self) {}
    /// Cap/NOMEM block ended.
    fn on_tx_block_sample(&self) {}
    /// tsflow sample trigger.
    fn on_tx_flow_sample(&self) {}
    /// TX log ring drop.
    fn on_tx_log_dropped(&self) {}
}
