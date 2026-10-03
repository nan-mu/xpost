//! Fixed IRAM / cold-table addresses from board intent `layout.toml`.
//!
//! Emitted by `build.rs` / `xpost-build` for **all** boot paths. Stub constants
//! when `XPOST_BOARD` is unset. bpf.toml boot images live in sibling modules
//! under [`super`] (feature `from-bpf-toml`), not here.

include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
