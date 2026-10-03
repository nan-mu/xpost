//! Fixed IRAM / cold-table addresses from board codegen.
//!
//! Emitted by firmware `xpost-build` sync (`metadata` + `partitions.csv`).
//! Stub constants when the library builds without board sync.

include!(concat!(env!("OUT_DIR"), "/mem_map.rs"));
