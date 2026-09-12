//! Multicore / `BPF_MAP_TYPE_PERCPU_ARRAY` extension.
//!
//! Incomplete — enabling Cargo feature `multicore` fails with `compile_error!`.
//! Remove the `compile_error!` when PERCPU backend exists.
//! First-case ESP32-C3 contract uses single-core ARRAY only.

#![cfg(feature = "multicore")]

compile_error!("feature `multicore` is not implemented yet; do not enable it");
