//! Early RX ingress (native-XDP driver surface).
//!
//! Feature `xdp` enables the full XDP action set on the Wi-Fi RX path:
//! [`Prog`] attach, Pass / Drop / same-iface zero-copy Tx, and counter callbacks.
//!
//! # Initialization order
//!
//! 1. (Optional) Call [`attach_prog`] **once**, before [`super::new`] / Wi-Fi start.
//! 2. Create the controller with [`super::new`] and start Wi-Fi as usual.
//! 3. Do **not** call [`attach_prog`] again. The slot is a one-shot
//!    [`embassy_sync::once_lock::OnceLock`]; a second attach fails and leaves
//!    the first prog mounted.
//!
//! If [`attach_prog`] is never called, RX uses [`NoopProg`] (always Pass).
//! There is no detach/reset API: a failed [`super::new`] may leave a prog
//! attached; retrying `new` without re-attach is fine.
//!
//! With `xdp` enabled, the STA/AP RX callbacks always call
//! [`Prog::run`]. Whether AOT/XDP actually runs is up to the prog
//! (e.g. unleased → `(Pass, frame)`).
//!
//! # Counter callbacks
//!
//! | Event | Driver |
//! |-------|--------|
//! | Drop path freed `eb` | [`Prog::count_drop`] once |
//! | Tx handoff failed and `eb` freed | [`Prog::count_failed_tx`] once |
//! | Tx handoff succeeded (`esp_wifi_internal_tx_by_ref` == OK) | no counter |
//!
//! # Tx failure conditions
//!
//! Null `eb`, empty window, ownership-token exhaustion, or non-OK
//! `esp_wifi_internal_tx_by_ref`.
//!
//! Netstack callbacks are registered in `wifi_init`; failure aborts init.
//!
//! Recommended typecheck: `esp32c3,wifi,unstable,xdp`.

use embassy_sync::once_lock::OnceLock;

use crate::sys::{c_types, include};

/// Disposition returned by [`Prog::run`].
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

/// Program mounted on the Wi-Fi RX ingress path.
///
/// `run` must return a subslice of `frame` (including the full slice).
/// Counter methods are invoked only by the driver after the corresponding
/// buffer disposition; implementations own all storage details.
pub trait Prog: Send + Sync {
    /// Inspect/modify `frame` and return an [`Action`] plus the visible window.
    ///
    /// The returned slice must be a subslice of `frame` (possibly the full
    /// frame). On [`Action::Pass`], the driver enqueues using this window's
    /// pointer and length (shrink-only `adjust_tail` takes effect here).
    fn run<'a>(&self, frame: &'a mut [u8]) -> (Action, &'a mut [u8]);

    /// Called once after the driver frees `eb` on the Drop path.
    fn count_drop(&self);

    /// Called once after a Tx attempt fails and `eb` has been freed (or
    /// handoff failed such that the ownership protocol will free it).
    fn count_failed_tx(&self);
}

/// Default prog: always Pass the full frame; counters are no-ops.
pub struct NoopProg;

impl Prog for NoopProg {
    #[inline]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (Action, &'a mut [u8]) {
        (Action::Pass, frame)
    }

    #[inline]
    fn count_drop(&self) {}

    #[inline]
    fn count_failed_tx(&self) {}
}

static NOOP: NoopProg = NoopProg;

/// One-shot mount point for STA/AP RX ingress. See module docs for order.
static CURRENT: OnceLock<&'static dyn Prog> = OnceLock::new();

/// Install `prog` for STA/AP RX ingress.
///
/// Must be called at most once, and before [`super::new`] / Wi-Fi start.
/// Returns `Err(prog)` if a prog is already attached (first wins).
pub fn attach_prog(prog: &'static dyn Prog) -> Result<(), &'static dyn Prog> {
    CURRENT.init(prog)
}

#[inline]
pub(crate) fn current() -> &'static dyn Prog {
    CURRENT.try_get().copied().unwrap_or(&NOOP)
}

#[inline]
pub(crate) fn free_rx_buffer(eb: *mut c_types::c_void) {
    if !eb.is_null() {
        unsafe { include::esp_wifi_internal_free_rx_buffer(eb) };
    }
}
