//! MCU XDP runtime: maps, helpers, IRAM slots, RX ingest.
//!
//! Current hardware instantiation is ESP32-C3 (RV32IMC). The helper trampoline
//! and AOT `jalr` invoke path are `cfg(target_arch = "riscv32")`. This crate
//! does not claim architecture-agnostic behavior.
//!
//! No ESP-IDF, no host libc.

#![no_std]

extern crate alloc;

pub mod ctx;
pub mod datapath;
pub mod helper;
mod log_gate;
pub mod map;
pub mod mem_map;
pub mod mount;
pub mod part;
pub mod prog_table;
pub mod program;
pub mod redirect;
pub(crate) mod rx_scratch;
pub(crate) mod sync;

pub mod driver;

pub use helper::helper_dispatcher;
pub use mount::{MountError, MountKind, try_rx_slot};
pub use part::{check_layout, load_boot_image, read_slot, xdp};
pub use prog_table::SlotId;
pub use program::{
    ProgramError, ProgramId, XDP_SLOT_COUNT, XDP_SLOT_LEN, XdpInstallCmd, hot_swap_rx,
    image as xdp_image, install as install_xdp, map_binds as xdp_map_binds, promote as promote_xdp,
    remove as remove_xdp, stage_raw_into as stage_raw_xdp, swap_rx,
};
