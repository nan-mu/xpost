//! Host serial / flash backend and App-mode wire protocol for xpost.
//!
//! # Features (no defaults)
//!
//! This crate enables **no features by default**. Dependents must opt in and
//! should set `default-features = false` explicitly when listing features:
//!
//! ```toml
//! # Host agent / tooling
//! xpost-serial = { path = "../xpost-serial", default-features = false, features = ["client"] }
//!
//! # Firmware / board examples
//! xpost-serial = { path = "../xpost-serial", default-features = false, features = ["server"] }
//! ```
//!
//! | Feature | Contents |
//! |---------|----------|
//! | _(always)_ | `r#type` — line protocol (`Command` / `Reply`), `no_std`, no `alloc` |
//! | `client` | host USB App UART + flash control plane |
//! | `server` | firmware parse request / build reply (Embassy task helpers) |
//! | `dev` | allow `client` + `server` together; memory harness under `cfg(test)` |
//!
//! Without `dev`, `client` and `server` are mutually exclusive.
//!
//! In-crate tests:
//!
//! ```bash
//! cargo test --features "dev,client,server"
//! ```

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
