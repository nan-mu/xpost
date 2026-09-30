//! Early RX ingress (native-XDP driver surface).
//!
//! Feature `xdp` enables the full XDP action set on the Wi-Fi RX path:
//! [`XdpProg`] passed to [`super::new`], Pass / Drop / same-iface zero-copy Tx.
//!
//! Traits live in [`xpost_type`]; this module owns install + driver [`Action`].
//! With `xdp-observe`, [`super::new`] takes [`EspRadioTrace`] (extends [`XdpProg`]).
//! Observation call-sites invoke [`current`] methods directly under
//! `xdp-observe` — no helper wrapper and no local TxStats / event-log storage.
//! Counter / Aggregate / Gauge / Trace storage lives in xpost `ext::perf` when
//! those features replace the trait defaults.
//!
//! | Feature | Role |
//! |---------|------|
//! | `xdp` | [`XdpProg`] via `wifi::new(..., prog)`; Pass / Drop / same-iface Tx |
//! | `xdp-observe` | compile EspRadioTrace call-sites on RX / TX paths |
//!
//! # Initialization
//!
//! Pass `prog` as the third argument of [`super::new`].
//! The slot is a one-shot [`embassy_sync::once_lock::OnceLock`]; a second
//! `new` with another prog fails to replace the first (first wins).
//!
//! Recommended typecheck: `esp32c3,wifi,unstable,xdp` (+ `xdp-observe` as needed).
//!
//! ```bash
//! cargo check -p esp-radio --features esp32c3,wifi,unstable,xdp
//! # or from xpost workspace: cargo check-esp-radio-xdp
//! ```

use embassy_sync::once_lock::OnceLock;

use crate::sys::{c_types, include};

pub mod prof;
pub mod tx;

pub use tx::{
    TX_KICK, TxPoll, in_flight, pending_len, poll, set_inflight_cap, set_tsflow,
};
pub use xpost_type::{EspRadioTrace, XdpAction, XdpProg};

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

/// Default prog: always Pass the full frame; EspRadioTrace hooks are no-ops.
pub struct NoopProg;

impl XdpProg for NoopProg {
    #[inline]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (XdpAction, &'a mut [u8]) {
        (XdpAction::Pass, frame)
    }
}

#[cfg(feature = "xdp-observe")]
impl EspRadioTrace for NoopProg {}

static NOOP: NoopProg = NoopProg;

#[cfg(feature = "xdp-observe")]
static CURRENT: OnceLock<&'static dyn EspRadioTrace> = OnceLock::new();
#[cfg(not(feature = "xdp-observe"))]
static CURRENT: OnceLock<&'static dyn XdpProg> = OnceLock::new();

/// Install `prog` for STA/AP RX ingress (called from [`super::new`]).
///
/// First wins; a second init leaves the first prog mounted.
#[cfg(feature = "xdp-observe")]
pub(crate) fn install_prog(prog: &'static dyn EspRadioTrace) {
    let _ = CURRENT.init(prog);
}

#[cfg(not(feature = "xdp-observe"))]
pub(crate) fn install_prog(prog: &'static dyn XdpProg) {
    let _ = CURRENT.init(prog);
}

#[cfg(feature = "xdp-observe")]
#[inline]
pub(crate) fn current() -> &'static dyn EspRadioTrace {
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
