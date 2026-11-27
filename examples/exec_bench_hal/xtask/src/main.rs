use std::env;
use std::process::{Command, ExitCode};

const WORKLOADS: [&str; 8] = [
    "noop",
    "prime",
    "log2",
    "copy",
    "fletcher32",
    "bsort",
    "strcmp",
    "matmul",
];

fn main() -> ExitCode {
    if env::args_os().len() != 1 {
        eprintln!(
            "Usage: cargo build-hal-all. Set CARGO_TARGET_DIR to change the output directory."
        );
        return ExitCode::FAILURE;
    }
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for workload in WORKLOADS {
        let bin = format!("exec_bench_hal_{workload}");
        println!("Building {bin}");
        let status = Command::new(&cargo)
            .args([
                "build-hal",
                "--locked",
                "--features",
                workload,
                "--bin",
                &bin,
            ])
            .status();
        match status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("{bin} build failed: {status}");
                return ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!("Could not run Cargo for {bin}: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
