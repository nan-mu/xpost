//! Feature-gated integration implementations.
//!
//! Incomplete / optional surfaces live here so [`crate::map`] / [`crate::xdp`] stay
//! the primary integrator API. Enable the matching Cargo feature to pull them in.
//!
//! | Module | Feature | Notes |
//! |--------|---------|-------|
//! | [`ringbuf`] | `ringbuf` | incomplete → `compile_error!` in kinds |
//! | [`hot_switch`] | `hot-switch` | incomplete → `compile_error!` |
//! | [`multicore`] | `multicore` | incomplete → `compile_error!` |
//!
//! Boot paths (`from-bpf-toml` / firmware-first) are not modules here: use
//! [`crate::map::MapBuilder`] / [`crate::xdp::XdpBuilder`] directly.

#[cfg(feature = "ringbuf")]
pub mod ringbuf;

#[cfg(feature = "hot-switch")]
pub mod hot_switch;

#[cfg(feature = "multicore")]
pub mod multicore;
