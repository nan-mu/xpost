//! Host serial / flash backend and App-mode wire protocol for xpost.
//!
//! | Feature | Contents |
//! |---------|----------|
//! | _(always)_ | `r#type` — line protocol (`Command` / `Reply`), `no_std`, no `alloc` |
//! | `client` | `client` — sync App UART + flash task, host USB |
//! | `server` (default) | `server` — parse request / build reply (firmware side) |
//! | `dev` | allow `client` + `server` together; memory harness only under `cfg(test)` |
//!
//! Without `dev`, `client` and `server` are mutually exclusive.

#![cfg_attr(not(feature = "client"), no_std)]

#[cfg(all(feature = "client", feature = "server", not(feature = "dev")))]
compile_error!("features `client` and `server` are mutually exclusive; enable `dev` to combine them");

pub mod r#type;

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

#[cfg(all(test, feature = "dev", feature = "client", feature = "server"))]
mod tests;

pub use r#type::{command, reply};

#[cfg(feature = "client")]
pub use client::{
    Chip, DEFAULT_XDP_PARTITION, FlashConfig, FlashResult, InstallOutcome, OpenOptions, Port,
    SerialHandler, XdpEntry, XdpTable,
};
