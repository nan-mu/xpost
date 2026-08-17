use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=layout.toml");
    println!("cargo:rerun-if-changed=mem.toml");
    println!("cargo:rerun-if-changed=bpf.toml");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let result = xtask::generate_runtime(&manifest_dir, &out_dir).expect("xtask generate_runtime");

    for w in &result.warnings {
        println!("cargo:warning={w}");
    }

    // rlib `rustc-link-arg` is ignored at the final binary link. Firmware
    // build.rs consumes this via DEP_XDP_MCU_CUSTOM_SECTIONS.
    println!(
        "cargo:custom-sections={}",
        result.custom_sections_path.display()
    );
}
