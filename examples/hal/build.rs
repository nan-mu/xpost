use std::env;
use std::path::PathBuf;

fn main() {
    linker_be_nice();

    println!("cargo:rerun-if-changed=config.toml");

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
                _ => (),
            },
            _ => std::process::exit(1),
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
