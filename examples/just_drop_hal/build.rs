fn main() {
    load_dotenv();
    linker_be_nice();
    // make sure linkall.x is the last linker script
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}

/// Read `.env` next to Cargo.toml into compile-time `option_env!` keys.
/// Accepts `WIFI_PASS` or `WIFI_PASSWORD` → rustc-env `WIFI_PASS`.
/// Process env wins over `.env` so agent/CI hosts can inject credentials.
fn load_dotenv() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-env-changed=WIFI_SSID");
    println!("cargo:rerun-if-env-changed=WIFI_PASS");
    println!("cargo:rerun-if-env-changed=WIFI_PASSWORD");
    let mut ssid = None;
    let mut pass = None;
    if let Ok(text) = std::fs::read_to_string(&path) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let val = val.trim().trim_matches('"').trim_matches('\'');
            match key {
                "WIFI_SSID" => ssid = Some(val.to_string()),
                "WIFI_PASS" | "WIFI_PASSWORD" => pass = Some(val.to_string()),
                _ => {}
            }
        }
    }
    if let Ok(v) = std::env::var("WIFI_SSID") {
        if !v.is_empty() {
            ssid = Some(v);
        }
    }
    if let Ok(v) = std::env::var("WIFI_PASS").or_else(|_| std::env::var("WIFI_PASSWORD")) {
        if !v.is_empty() {
            pass = Some(v);
        }
    }
    if let Some(v) = ssid {
        println!("cargo:rustc-env=WIFI_SSID={v}");
    }
    if let Some(v) = pass {
        println!("cargo:rustc-env=WIFI_PASS={v}");
    }
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
