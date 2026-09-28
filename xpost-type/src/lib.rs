//! Shared XDP program / esp-radio-trace types for esp-radio ↔ xpost.
//!
//! `no_std`, no chip deps. Enable `defmt` for [`XdpAction`] formatting.
//!
//! Hierarchy: [`EspRadioTrace`]: [`XdpProg`] — performance hooks extend a program.
//! A bare [`XdpProg`] need not implement [`EspRadioTrace`]. Storage lives in xpost
//! (`ext::perf`); only the mounted prog is the trait object esp-radio calls.
//!
//! Hook names match the Phase-1 metric catalog (no `on_` prefix).

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

/// Performance hooks layered on a mounted [`XdpProg`] for esp-radio call-sites.
///
/// Names align with the metric catalog. The driver only **calls** these (when
/// `xdp-observe` is enabled); Counter / Aggregate / Gauge / Trace storage is
/// owned by the xpost implementor.
pub trait EspRadioTrace: XdpProg {
    /// `rx_packets` — RX software entry (`len` = frame length).
    fn rx_packets(&self, _len: usize) {}
    /// `xdp_invocations` — about to call [`XdpProg::run`].
    fn xdp_invocations(&self) {}
    /// `xdp_pass` / `xdp_drop` / `xdp_tx` / `xdp_aborted` — after [`XdpProg::run`].
    fn xdp_action(&self, _action: XdpAction) {}
    /// `rx_delivered` — Pass queued to stack (`queue_depth` after push).
    fn rx_delivered(&self, _queue_depth: usize) {}
    /// `rx_discarded` — Drop after free_rx_buffer.
    fn rx_discarded(&self) {}
    /// `rx_queue_overflow` — Pass path: RX queue full.
    fn rx_queue_overflow(&self, _queue_depth: usize) {}
    /// End of one RX dispatch (Aggregate / Trace `rx_end`).
    fn rx_end(&self) {}

    /// Aggregate `tx_submission` start — `tx_by_ref` about to run.
    fn tx_submission_begin(&self) {}
    /// `tx_submitted` — `tx_by_ref` OK.
    fn tx_submitted(&self) {}
    /// `tx_failed` — `tx_by_ref` failed.
    fn tx_failed(&self) {}
    /// `tx_completed` / `tx_air_done_fail` — air TX completion (`ok` = success).
    fn tx_completed(&self, _ok: bool) {}
    /// `tx_fail_bad_buf`.
    fn tx_fail_bad_buf(&self) {}
    /// `tx_fail_token`.
    fn tx_fail_token(&self) {}
    /// `tx_grp_src`.
    fn tx_grp_src(&self) {}
    /// `tx_enq`.
    fn tx_enq(&self) {}
    /// `tx_enq_fail_claim`.
    fn tx_enq_fail_claim(&self) {}
    /// `tx_enq_fail_ring`.
    fn tx_enq_fail_ring(&self) {}
    /// `tx_kick`.
    fn tx_kick(&self) {}
    /// `tx_poll`.
    fn tx_poll(&self) {}
    /// `tx_cap_wait`.
    fn tx_cap_wait(&self) {}
    /// `tx_nomem_wait`.
    fn tx_nomem_wait(&self) {}
    /// `tx_no_retain`.
    fn tx_no_retain(&self) {}
    /// `tx_flush`.
    fn tx_flush(&self) {}
    /// `tx_ns_ref`.
    fn tx_ns_ref(&self) {}
    /// `tx_ns_free`.
    fn tx_ns_free(&self) {}
    /// `tx_queue_depth` / `tx_inflight` gauges (`pending`, `inflight`).
    fn tx_depth(&self, _pending: usize, _inflight: usize) {}
    /// Aggregate `tx_block` (cap/NOMEM block ended).
    fn tx_block(&self) {}
    /// `tx_flow_sample`.
    fn tx_flow_sample(&self) {}
    /// `tx_log_dropped`.
    fn tx_log_dropped(&self) {}
}
