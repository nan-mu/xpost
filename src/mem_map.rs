//! Fixed IRAM / cold-table addresses from board intent `layout.toml`.
//!
//! Emitted by `build.rs` / `xpost-build` for **both** boot paths
//! (`from-bpf-toml` and `xdp-firmware-first`). Stub constants when
//! `XPOST_BOARD` is unset. bpf.toml boot images live under
//! `xdp::table::boot` / `map::runtime::boot`, not here.

include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
