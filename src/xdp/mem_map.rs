//! Fixed IRAM / cold-table addresses from board intent `layout.toml`.
//!
//! Emitted by `build.rs` / `xpost-build` for every boot path. Stub constants
//! when `XPOST_BOARD` is unset.

include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
