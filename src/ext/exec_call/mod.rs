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
#[esp_hal::ram]
pub fn invoke(buf: &mut [u8]) -> (XdpAction, &mut [u8]) {
    crate::xdp::ingress::run(buf)
}
