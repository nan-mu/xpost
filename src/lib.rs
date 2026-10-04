//! xpost — XDP datapath on MCU (*express post*): AOT invoke, maps, helpers, IRAM slots, RX ingest.
//!
//! Integrator-facing surface is primarily [`map`] and [`xdp`]:
//! - [`map::MapRt`] / [`map::MapBuilder`]
//! - [`xdp::XdpRt`] / [`xdp::XdpBuilder`]
//!
//! Optional performance monitoring: enable `perf-counter` / `perf-aggregate` /
//! `perf-gauge` / `perf-trace` and read via [`ext::perf`]. Wi-Fi + XDP attach:
//! enable `esp-radio` and call [`esp_radio::new`]. Function-call invoke of a
//! leased image (no network RX): enable `exec-call` and call
//! [`ext::exec_call::invoke`].
//!
//! Control-plane errors live in [`error`] (one small enum per public entrypoint).
//! Optional / incomplete feature surface: [`ext`] (`ringbuf`, `exec_call`). Board
//! codegen is **`xpost-build`** (firmware-side). Boot is firmware-first:
//! [`xdp::XdpRt::init`] scans Flash XDP ELF, creates required maps, then
//! [`xdp::XdpRt::lease`] installs VMR aliases from image meta.
//!
//! Current hardware instantiation is ESP32-C3 (RV32IMC). This crate does not claim
//! architecture-agnostic behavior until additional targets exist. No ESP-IDF, no
//! host libc.
//!
//! # Layout
//!
//! ```text
//! xpost/                          # this repo (publishable library at root)
//!   src/                          # xpost runtime
//!   xpost-build/                  # board codegen (layout/partitions → Rust + IRAM linker script)
//!   xtask/                        # root dispatcher only
//!   examples/
//!     just_drop/                  # firmware-first cold XDP ELF path
//! ```
//!
//! - **Library**: does not ship board tomls. Integrators set board metadata /
//!   `partitions.csv` via the firmware crate.
//! - **Each example**: independent MCU project with its own xtask; can be copied
//!   out of tree.
//! - **`mem.toml` is obsolete**. Codegen consumes in-memory layout from
//!   `[package.metadata.xpost]` ∪ board files. The libbpf-mcu **contract**
//!   `layout.toml` (§3.2) is exported under `target/layout-export/`.
//!
//! # Boot
//!
//! Firmware-first only: [`xdp::XdpRt::init`] scans Flash XDP ELF, creates required
//! maps; [`xdp::XdpRt::lease`] installs VMR aliases from image meta.
//!
//! Soft errors / driver counters: enable `perf-counter` and read
//! [`ext::perf::counter`]. Wi-Fi+XDP: `esp-radio` feature + [`esp_radio::new`].
//! Optional `perf-aggregate` / `perf-gauge` / `perf-trace`. Non-network call:
//! `exec-call` + [`ext::exec_call::invoke`]. Control-plane APIs return `Result`.
//!
//! # Commands (host, repo root)
//!
//! ```bash
//! cargo xtask just_xdp build --release
//! cargo xtask agent program --elf examples/just_xdp/just_drop.elf
//! ```
//!
//! # AOT handoff (from libbpf-mcu)
//!
//! Build / encode an XDP ELF (host: `xpost-compact-elf` + libbpf-mcu `bpf.toml` for
//! map meta). Cold-install via agent. For firmware layout export:
//! `cargo xtask sync export --board <example>`.

#![no_std]

extern crate alloc;
extern crate self as xpost;

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    mutex::{Mutex, MutexGuard},
};

/// Wi-Fi bring-up injecting the crate default XDP program into `::esp_radio::wifi::new`.
#[cfg(feature = "esp-radio")]
pub mod esp_radio {
    pub use crate::xdp::ingress::esp_radio::new;
}
/// App-mode control helpers for board examples (`enable xdp`, map/perf ops).
pub mod app_ctrl;
/// Control-plane errors (map + XDP public entrypoints).
pub mod error;
/// Feature-gated optional surfaces (`ringbuf`, `ext::perf`, `ext::exec_call`).
pub mod ext;
/// AOT helper trampoline — not part of the integrator API; see [`helper::helper_dispatcher`].
#[doc(hidden)]
pub mod helper;
pub mod map;
pub mod xdp;

/// Shared critical-section mutex for map / XDP runtimes.
pub type RtMutex<T> = Mutex<CriticalSectionRawMutex, T>;
/// Guard for [`RtMutex`].
pub type RtMutexGuard<'a, T> = MutexGuard<'a, CriticalSectionRawMutex, T>;

/// Locked [`xdp::XdpRt`] (also re-exported as [`xdp::XdpRtGuard`]).
pub type XdpRtGuard<'a> = xdp::XdpRtGuard<'a>;
/// Locked [`map::MapRt`] (also re-exported as [`map::MapRtGuard`]).
pub type MapRtGuard<'a> = map::MapRtGuard<'a>;
