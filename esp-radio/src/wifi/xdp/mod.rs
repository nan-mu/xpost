//! Early RX ingress (native-XDP driver surface).
//!
//! Feature `xdp` enables the full XDP action set on the Wi-Fi RX path:
//! [`XdpProg`] passed to [`super::new`], Pass / Drop / same-iface zero-copy Tx.
//!
//! Traits live in [`xpost_type`]; this module owns install + driver [`Action`].
//! With `xdp-observe`, [`super::new`] takes [`Observe`] (extends [`XdpProg`]).
//!
//! # Initialization
//!
//! Pass `prog` as the third argument of [`super::new`].
//! The slot is a one-shot [`embassy_sync::once_lock::OnceLock`]; a second
//! `new` with another prog fails to replace the first (first wins).
//!
//! Recommended typecheck: `esp32c3,wifi,unstable,xdp` (+ `xdp-observe` as needed).

use embassy_sync::once_lock::OnceLock;

use crate::sys::{c_types, include};

pub mod observe;
pub mod prof;
pub mod tx;

pub use tx::{
    TxEvKind, TxLogEntry, TxPoll, TxStats, clear_stats, in_flight, last_drain_rc, pending_len,
    poll, set_inflight_cap, set_tsflow, snapshot_log, stats, TX_KICK,
};
pub use xpost_type::{Observe, XdpAction, XdpProg};

/// Driver disposition after mapping [`XdpAction`] (Pass / Drop / Tx).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Action {
    /// Enqueue the returned window on the normal RX path.
    Pass,
    /// Free the RX buffer; do not enqueue.
    Drop,
    /// Zero-copy same-iface transmit of the returned window.
    Tx,
}

/// Map program [`XdpAction`] + window onto driver [`Action`].
///
/// Invalid windows (not a prefix of the RX `frame`) become [`Action::Drop`].
#[inline]
pub(crate) fn map_xdp_action(
    xdp: XdpAction,
    frame_ptr: *const u8,
    frame_len: usize,
    window: &[u8],
) -> Action {
    let valid = (window.as_ptr() == frame_ptr) & (window.len() <= frame_len);
    if !valid {
        return Action::Drop;
    }
    match xdp {
        XdpAction::Aborted | XdpAction::Drop => Action::Drop,
        XdpAction::Pass => Action::Pass,
        XdpAction::Tx => Action::Tx,
    }
}

/// Default prog: always Pass the full frame; observe hooks are no-ops.
pub struct NoopProg;

impl XdpProg for NoopProg {
    #[inline]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (XdpAction, &'a mut [u8]) {
        (XdpAction::Pass, frame)
    }
}

#[cfg(feature = "xdp-observe")]
impl Observe for NoopProg {}

static NOOP: NoopProg = NoopProg;

#[cfg(feature = "xdp-observe")]
static CURRENT: OnceLock<&'static dyn Observe> = OnceLock::new();
#[cfg(not(feature = "xdp-observe"))]
static CURRENT: OnceLock<&'static dyn XdpProg> = OnceLock::new();

/// Install `prog` for STA/AP RX ingress (called from [`super::new`]).
///
/// First wins; a second init leaves the first prog mounted.
#[cfg(feature = "xdp-observe")]
pub(crate) fn install_prog(prog: &'static dyn Observe) {
    let _ = CURRENT.init(prog);
}

#[cfg(not(feature = "xdp-observe"))]
pub(crate) fn install_prog(prog: &'static dyn XdpProg) {
    let _ = CURRENT.init(prog);
}

#[cfg(feature = "xdp-observe")]
#[inline]
pub(crate) fn current() -> &'static dyn Observe {
    CURRENT.try_get().copied().unwrap_or(&NOOP)
}

#[cfg(not(feature = "xdp-observe"))]
#[inline]
pub(crate) fn current() -> &'static dyn XdpProg {
    CURRENT.try_get().copied().unwrap_or(&NOOP)
}

#[inline]
pub(crate) fn free_rx_buffer(eb: *mut c_types::c_void) {
    if !eb.is_null() {
        unsafe { include::esp_wifi_internal_free_rx_buffer(eb) };
    }
}
