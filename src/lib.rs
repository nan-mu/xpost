//! xpost — XDP datapath on MCU (express post): maps, helpers, IRAM slots, RX ingest.
//!
//! Board layout (`layout.toml` / `mem.toml` / `bpf.toml`) is **not** part of this
//! crate. Integrators set `XPOST_BOARD` to a board directory (see `examples/firmware`).
//!
//! Current hardware instantiation is ESP32-C3 (RV32IMC). The helper trampoline
//! and AOT `jalr` invoke path are `cfg(target_arch = "riscv32")`. This crate
//! does not claim architecture-agnostic behavior.
//!
//! No ESP-IDF, no host libc.

#![no_std]

extern crate alloc;
extern crate self as xpost;

pub mod action;
pub mod ctx;
pub mod helper;
mod log_gate;
pub mod map;
pub mod mem_map;
pub mod prog_table;
pub mod xdp;

pub use map::MapRtGuard;
pub use xdp::{RtMutex, RtMutexGuard, XdpRtGuard};

#[cfg(feature = "event-ringbuf")]
pub use map::ringbuf::evt;

pub use helper::helper_dispatcher;
pub use prog_table::SlotId;
