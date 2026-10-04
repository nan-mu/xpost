//! Feature-gated integration implementations.
//!
//! Incomplete / optional surfaces live here so [`crate::map`] / [`crate::xdp`] stay
//! the primary integrator API. Enable the matching Cargo feature to pull them in.
//!
//! | Module | Feature | Notes |
//! |--------|---------|-------|
//! | [`ringbuf`] | `ringbuf` | incomplete → `compile_error!` |
//! | [`perf`] | `perf-counter` / `perf-aggregate` / `perf-gauge` / `perf-trace` | EspRadioTrace backends |
//! | [`exec_call`] | `exec-call` | function-call invoke of leased XDP image (non-network) |
//!
//! Boot is firmware-first. Use [`crate::map::MapBuilder`] / [`crate::xdp::XdpBuilder`] directly.

#[cfg(feature = "ringbuf")]
pub mod ringbuf;

#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace"
))]
pub mod perf;

#[cfg(feature = "exec-call")]
pub mod exec_call;
