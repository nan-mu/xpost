//! xpost — XDP datapath on MCU (express post): maps, helpers, IRAM slots, RX ingest.
//!
//! Integrator-facing surface is primarily [`map`] and [`xdp`]:
//! - [`map::MapRt`] / [`map::MapBuilder`] / [`map::declare_runtime`]
//! - [`xdp::XdpRt`] / [`xdp::XdpBuilder`]
//!
//! Optional / incomplete feature surfaces live under [`ext`] (`ringbuf`, `hot-switch`,
//! `multicore`). Board codegen is **`xpost-build`**. Features `from-bpf-toml` and
//! `xdp-firmware-first` are mutually exclusive.
//!
//! Current hardware instantiation is ESP32-C3 (RV32IMC). No ESP-IDF, no host libc.

#![no_std]

#[cfg(all(feature = "from-bpf-toml", feature = "xdp-firmware-first"))]
compile_error!("features `from-bpf-toml` and `xdp-firmware-first` are mutually exclusive");

extern crate alloc;
extern crate self as xpost;

pub mod ctx;
/// Feature-gated optional surfaces (`ringbuf`, `hot-switch`, `multicore`).
pub mod ext;
/// AOT helper trampoline — not part of the integrator API; see [`helper::helper_dispatcher`].
#[doc(hidden)]
pub mod helper;
mod log_gate;
pub mod map;
pub mod xdp;

/// Board layout constants (`layout.toml` → `OUT_DIR/mem_map.rs`).
pub use xdp::mem_map;
