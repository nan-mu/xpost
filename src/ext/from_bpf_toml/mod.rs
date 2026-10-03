//! bpf.toml boot (`feature = "from-bpf-toml"`).
//!
//! Firmware includes generated `BOOT_MAPS` / `BOOT_XDPS` and calls
//! [`crate::map::MapBuilder::from_bpf_toml`] / [`crate::xdp::XdpBuilder::from_bpf_toml`].

/// Re-export declare_runtime for path B firmware roots.
pub use xpost_macros::declare_runtime;
