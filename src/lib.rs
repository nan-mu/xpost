//! xpost — XDP datapath on MCU (express post): maps, helpers, IRAM slots, RX ingest.
//!
//! Integrator-facing surface is primarily [`map`] and [`xdp`]:
//! - [`map::MapRt`] / [`map::MapBuilder`]
//! - [`xdp::XdpRt`] / [`xdp::XdpBuilder`]
//!
//! Optional / incomplete feature surface: [`ext`] (`ringbuf`). Board codegen is
//! **`xpost-build`**. Boot is firmware-first: [`xdp::XdpRt::init`] scans Flash
//! Compact ELF (XDP ELF), creates required maps, then [`xdp::XdpRt::lease`]
//! installs VMR aliases from image meta.
//!
//! Current hardware instantiation is ESP32-C3 (RV32IMC). No ESP-IDF, no host libc.

#![no_std]

extern crate alloc;
extern crate self as xpost;

/// Wi-Fi bring-up with native-XDP `::esp_radio::wifi::Prog` attach.
pub mod esp_radio {
    pub use crate::xdp::ingress::esp_radio::{XDP_PROG, XdpProg, new};
}
/// Feature-gated optional surfaces (`ringbuf`).
pub mod ext;
/// AOT helper trampoline — not part of the integrator API; see [`helper::helper_dispatcher`].
#[doc(hidden)]
pub mod helper;
pub mod map;
pub mod xdp;

/// Board layout constants (firmware `xpost-build` sync → `OUT_DIR/mem_map.rs`).
pub use xdp::mem_map;
