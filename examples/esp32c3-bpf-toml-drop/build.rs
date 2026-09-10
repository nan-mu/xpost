use std::env;
use std::path::PathBuf;

fn main() {
    linker_be_nice();

    println!("cargo:rerun-if-changed=esp32c3.bin");
    println!("cargo:rerun-if-changed=config.toml");
    println!("cargo:rerun-if-changed=layout.toml");
    println!("cargo:rerun-if-changed=mem.toml");
    println!("cargo:rerun-if-changed=bpf.toml");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let result =
        xpost_build::generate_fw_cfg(&manifest_dir, &out_dir).expect("xpost-build generate_fw_cfg");

    for name in xpost_build::US_CFGS {
        println!("cargo:rustc-check-cfg=cfg(fw_us_{name})");
    }
    for name in &result.enabled_us_cfgs {
        println!("cargo:rustc-cfg=fw_us_{name}");
    }
    for w in &result.warnings {
        println!("cargo:warning={w}");
    }

    println!("cargo:rustc-link-arg=-Tlinkall.x");
    let sections = env::var("DEP_XPOST_CUSTOM_SECTIONS")
        .expect("xpost build.rs must emit cargo:custom-sections= (set XPOST_BOARD)");
    println!("cargo:rustc-link-arg=-T{sections}");
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
