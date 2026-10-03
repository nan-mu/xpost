//! Host serial / flash backend and App-mode wire protocol for xpost.
//!
//! | Feature | Contents |
//! |---------|----------|
//! | _(always)_ | `r#type` — line protocol (`Command` / `Reply`), `no_std`, no `alloc` |
//! | `client` (default) | `client` — sync App UART + flash task, host USB |
//! | `server` | `server` — parse request / build reply (firmware side) |
//!
//! `client` and `server` are mutually exclusive.

#![cfg_attr(not(feature = "client"), no_std)]

#[cfg(all(feature = "client", feature = "server"))]
compile_error!("features `client` and `server` are mutually exclusive");

pub mod r#type;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

pub use r#type::{command, reply};

#[cfg(feature = "client")]
pub use client::{
    Chip, DEFAULT_XDP_PARTITION, FlashConfig, FlashResult, InstallOutcome, OpenOptions, Port,
    SerialHandler, XdpEntry, XdpTable,
};
