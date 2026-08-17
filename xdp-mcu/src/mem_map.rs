//! Fixed IRAM addresses for AOT helpers / hot-load (from `mem.toml`).

include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
