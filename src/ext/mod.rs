//! Feature-gated integration implementations.
//!
//! Incomplete / optional surfaces live here so [`crate::map`] / [`crate::xdp`] stay
//! the primary integrator API. Enable the matching Cargo feature to pull them in.
//!
//! | Module | Feature | Notes |
//! |--------|---------|-------|
//! | [`ringbuf`] | `ringbuf` | incomplete → `compile_error!` in kinds |
//!
//! Boot is firmware-first. Use [`crate::map::MapBuilder`] / [`crate::xdp::XdpBuilder`] directly.

#[cfg(feature = "ringbuf")]
pub mod ringbuf;
