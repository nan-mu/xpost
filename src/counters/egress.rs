//! Legacy XDP_TX diagnostic counters.
//!
//! Zero-copy Tx ownership now lives in `esp-radio` (`rx-ingress`).
//! These counters remain for optional diagnostics;
//! `EGRESS.tx_by_ref` is also bumped from
//! [`crate::xdp::ingress::esp_radio::AotXdpProg::count_failed_tx`].

use portable_atomic::{AtomicUsize, Ordering};

/// Per-cause counters for XDP_TX / token lifetime (Relaxed; diagnostic only).
pub struct EgressStats {
    /// `arm_zero_copy` failed to register netstack callbacks.
    pub arm_callbacks: AtomicUsize,
    /// `transmit` saw a null `eb`.
    pub null_eb: AtomicUsize,
    /// Packet window length was zero.
    pub clip_empty: AtomicUsize,
    /// Window `data` pointer is before the RX frame base.
    pub clip_before_frame: AtomicUsize,
    /// Window end overflows or extends past the RX frame.
    pub clip_oob: AtomicUsize,
    /// Netstack callbacks were never armed successfully.
    pub callbacks_not_ready: AtomicUsize,
    /// Ownership tokens exhausted (historical; driver-owned now).
    pub token_exhausted: AtomicUsize,
    /// Driver reported a failed Tx handoff via prog `count_failed_tx`.
    pub tx_by_ref: AtomicUsize,
    /// Successful `tx_by_ref` handoff (async TX may still be in flight).
    pub tx_ok: AtomicUsize,
    /// Wi-Fi `netstack_ref` with a pointer not in the token pool.
    pub ref_unknown_token: AtomicUsize,
    /// Wi-Fi `netstack_free` with a pointer not in the token pool.
    pub free_unknown_token: AtomicUsize,
    /// `retain_tx` while slot is not owned.
    pub retain_inactive: AtomicUsize,
    /// `retain_tx` while `refs == 0`.
    pub retain_unref: AtomicUsize,
    /// `release` while `refs == 0`.
    pub release_dup: AtomicUsize,
    /// Final `release` while slot is not owned.
    pub release_inactive: AtomicUsize,
}

impl EgressStats {
    const fn new() -> Self {
        Self {
            arm_callbacks: AtomicUsize::new(0),
            null_eb: AtomicUsize::new(0),
            clip_empty: AtomicUsize::new(0),
            clip_before_frame: AtomicUsize::new(0),
            clip_oob: AtomicUsize::new(0),
            callbacks_not_ready: AtomicUsize::new(0),
            token_exhausted: AtomicUsize::new(0),
            tx_by_ref: AtomicUsize::new(0),
            tx_ok: AtomicUsize::new(0),
            ref_unknown_token: AtomicUsize::new(0),
            free_unknown_token: AtomicUsize::new(0),
            retain_inactive: AtomicUsize::new(0),
            retain_unref: AtomicUsize::new(0),
            release_dup: AtomicUsize::new(0),
            release_inactive: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn bump(counter: &AtomicUsize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// Global egress diagnostic counters.
pub static EGRESS: EgressStats = EgressStats::new();
