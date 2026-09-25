//! xpost library build: stub linker fragment for host / bare-lib checks.
//!
//! Board codegen (`mem_map.rs`, slots) lives in the firmware `build.rs`
//! (`xpost-build` sync). This crate does not include board layout.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(
        out_dir.join("custom-sections.x"),
        "/* stub — firmware sync emits real linker fragment */\n",
    )
    .unwrap();
    println!(
        "cargo:custom-sections={}",
        out_dir.join("custom-sections.x").display()
    );
}
