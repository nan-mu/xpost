//! Function-call invoke of a leased XDP image.
//!
//! Enable Cargo feature `exec-call`. This is an additive [`crate::ext`] surface:
//! firmware prepares a buffer and calls [`invoke`]; the call reuses the existing
//! lease flight in [`crate::xdp::ingress`] (`jalr` into IRAM). It is **not** a
//! second network ingress attach — Wi-Fi RX remains behind `esp-radio`.

use xpost_type::XdpAction;

/// Run the currently leased XDP image on `buf`.
///
/// Same contract as the network datapath: returns a Linux-style [`XdpAction`]
/// and a visible window (subslice of `buf`). When no image is leased, returns
/// [`XdpAction::Pass`] and the full buffer (inactive path).
///
/// When perf backends are compiled in, this records the same XDP begin/end
/// hooks as Wi-Fi ingress (`xdp_invocations` / `xdp_action`) so `get perf`
/// shows one execution per `enable exec`.
#[esp_hal::ram]
pub fn invoke(buf: &mut [u8]) -> (XdpAction, &mut [u8]) {
    #[cfg(any(
        feature = "perf-counter",
        feature = "perf-aggregate",
        feature = "perf-trace"
    ))]
    {
        use xpost_type::EspRadioTrace;
        crate::xdp::ingress::XDP_PROG.xdp_invocations();
    }
    let (act, window) = crate::xdp::ingress::run(buf);
    #[cfg(any(
        feature = "perf-counter",
        feature = "perf-aggregate",
        feature = "perf-trace"
    ))]
    {
        use xpost_type::EspRadioTrace;
        crate::xdp::ingress::XDP_PROG.xdp_action(act);
    }
    (act, window)
}
