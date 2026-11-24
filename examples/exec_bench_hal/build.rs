fn main() {
    linker_be_nice();
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    build_exb2();
}

fn build_exb2() {
    // Sources from libbpf-mcu 07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113.
    println!("cargo:rerun-if-changed=exb2");
    println!("cargo:rerun-if-env-changed=EXB2_CLANG");
    println!("cargo:rerun-if-env-changed=PATH");
    let compiler = std::env::var_os("EXB2_CLANG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(riscv_clang);
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    for task in 0..7 {
        let symbol = format!("exb2_hal_{task}");
        let task_dir = out_dir.join(&symbol);
        std::fs::create_dir_all(&task_dir).unwrap();
        let objects = cc::Build::new()
            .compiler(&compiler)
            .out_dir(task_dir)
            .no_default_flags(true)
            .flag("--target=riscv32-unknown-none-elf")
            .flag("-march=rv32imc")
            .flag("-mabi=ilp32")
            .flag("-ffreestanding")
            .flag("-fno-builtin")
            .flag("-Os")
            .define("EB_HOST_ENTRY", None)
            .define("EB_TASK_ID", task.to_string().as_str())
            .define("exec_entry", symbol.as_str())
            .file("exb2/exec_program.h")
            .flag("-x")
            .flag("c")
            .compile_intermediates();
        for object in objects {
            // Link the ELF object directly. The host archiver may use Mach-O.
            println!(
                "cargo:rustc-link-arg-bin=exec_bench_hal={}",
                object.display()
            );
        }
    }
}

fn riscv_clang() -> std::path::PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("clang"))
        .filter(|compiler| compiler.is_file())
        .find(|compiler| {
            std::process::Command::new(compiler)
                .arg("--print-targets")
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .split_ascii_whitespace()
                            .any(|target| target == "riscv32")
                })
        })
        .expect("EXB2 HAL needs RISC-V Clang in PATH or EXB2_CLANG")
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
