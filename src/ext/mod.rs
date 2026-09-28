//! Feature-gated integration implementations.
//!
//! Incomplete / optional surfaces live here so [`crate::map`] / [`crate::xdp`] stay
//! the primary integrator API. Enable the matching Cargo feature to pull them in.
//!
//! | Module | Feature | Notes |
//! |--------|---------|-------|
//! | [`ringbuf`] | `ringbuf` | incomplete → `compile_error!` |
//! | [`trace`] | `trace-counter` / `trace-timestamp` | Observe / ring storage for [`crate::XDP_PROG`] |
//!
//! Boot is firmware-first. Use [`crate::map::MapBuilder`] / [`crate::xdp::XdpBuilder`] directly.

#[cfg(feature = "ringbuf")]
pub mod ringbuf;

#[cfg(any(feature = "trace-counter", feature = "trace-timestamp"))]
pub mod trace;
