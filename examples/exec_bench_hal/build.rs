use xpost_build::firmware_codegen;

fn main() {
    linker_be_nice();
    firmware_codegen().unwrap();
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];
        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!("💡 `defmt` not found — add defmt.x / use defmt_rtt");
                }
                "_stack_start" => eprintln!("💡 Is the linker script `linkall.x` missing?"),
                what if what.starts_with("esp_rtos_") => {
                    eprintln!("💡 Initialize `esp-rtos` (embassy scheduler).");
                }
                "free" | "malloc" | "calloc" | "get_free_internal_heap_size"
                | "malloc_internal" | "realloc_internal" | "calloc_internal"
                | "free_internal" => {
                    eprintln!("💡 Missing `esp-alloc`?");
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
