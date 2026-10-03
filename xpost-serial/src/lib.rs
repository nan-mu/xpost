//! Host serial / flash backend and App-mode wire protocol for xpost.
//!
//! | Feature | Contents |
//! |---------|----------|
//! | _(always)_ | `r#type` — line protocol (`Command` / `Reply`) |
//! | `client` (default) | `client` — sync App UART + flash task, host USB |
//! | `server` | `server` — parse request / build reply (DUT side) |
//!
//! `client` and `server` are mutually exclusive.

#![cfg_attr(not(feature = "client"), no_std)]

extern crate alloc;

#[cfg(all(feature = "client", feature = "server"))]
compile_error!("features `client` and `server` are mutually exclusive");

pub mod r#type;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

pub use r#type::{command, ops, reply};

#[cfg(feature = "client")]
pub use client::{
    DEFAULT_XDP_PARTITION, Chip, Client, FlashConfig, FlashHandle, FlashResult, InstallOutcome,
    OpenOptions, Port, XdpEntry, XdpTable, xdp_name_from_bytes, xdp_name_from_file,
    xdp_table_from_csv,
};
