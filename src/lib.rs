//! xpost — XDP datapath on MCU (express post): maps, helpers, IRAM slots, RX ingest.
//!
//! Integrator-facing surface is primarily [`map`] and [`xdp`]:
//! - [`map::MapRt`] / [`map::MapBuilder`]
//! - [`xdp::XdpRt`] / [`xdp::XdpBuilder`]
//!
//! Optional observation: enable `trace-counter` / `trace-timestamp` and use [`XDP_PROG`].
//! Wi-Fi + XDP attach: enable `esp-radio` and call [`esp_radio::new`].
//!
//! Control-plane errors live in [`error`] (one small enum per public entrypoint).
//! Optional / incomplete feature surface: [`ext`] (`ringbuf`). Board codegen is
//! **`xpost-build`** (firmware-side). Boot is firmware-first: [`xdp::XdpRt::init`]
//! scans Flash Compact ELF (XDP ELF), creates required maps, then
//! [`xdp::XdpRt::lease`] installs VMR aliases from image meta.
//!
//! Current hardware instantiation is ESP32-C3 (RV32IMC). No ESP-IDF, no host libc.

#![no_std]

extern crate alloc;
extern crate self as xpost;

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    mutex::{Mutex, MutexGuard},
};

/// Wi-Fi bring-up injecting [`XDP_PROG`] into `::esp_radio::wifi::new`.
#[cfg(feature = "esp-radio")]
pub mod esp_radio {
    pub use crate::xdp::ingress::esp_radio::new;
}
/// Control-plane errors (map + XDP public entrypoints).
pub mod error;
/// Feature-gated optional surfaces (`ringbuf`, `ext::trace` backends).
pub mod ext;
/// AOT helper trampoline — not part of the integrator API; see [`helper::helper_dispatcher`].
#[doc(hidden)]
pub mod helper;
pub mod map;
/// Always-on [`XDP_PROG`] ([`xpost_trait::XdpProg`] + optional Observe / timestamps).
pub mod trace;
pub use trace::XDP_PROG;
pub mod xdp;

/// Shared critical-section mutex for map / XDP runtimes.
pub type RtMutex<T> = Mutex<CriticalSectionRawMutex, T>;
/// Guard for [`RtMutex`].
pub type RtMutexGuard<'a, T> = MutexGuard<'a, CriticalSectionRawMutex, T>;

/// Locked [`xdp::XdpRt`] (also re-exported as [`xdp::XdpRtGuard`]).
pub type XdpRtGuard<'a> = xdp::XdpRtGuard<'a>;
/// Locked [`map::MapRt`] (also re-exported as [`map::MapRtGuard`]).
pub type MapRtGuard<'a> = map::MapRtGuard<'a>;
