//! Host linker wrapper: `rust-lld` then embed `.xpost.mem` (XPM1).
//!
//! Selected via board `.cargo/config.toml`:
//! `[target.riscv32imc-unknown-none-elf] linker = "…/xpost-link"`
//! (Cargo 1.100+ currently ignores `cargo:rustc-linker` from build scripts.)

use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    xpost_build::link_wrap::main_linker(&args)
}
