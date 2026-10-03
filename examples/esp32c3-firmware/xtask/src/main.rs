//! Example-local xtask: doctor / map / gen / layout-export for this MCU board.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "examples/esp32c3-firmware board codegen"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate intent layout.toml and write contract under target/layout-export/.
    Doctor,
    /// Deprecated alias for `doctor`.
    Layout,
    /// Print IBUS/DBUS dual-map from intent layout.toml.
    Map,
    /// Export §3.2 contract layout.toml (does not overwrite board intent).
    LayoutExport {
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Write generated artifacts to `--out`.
    Gen {
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn board_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask parent = examples/esp32c3-firmware")
        .to_path_buf()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let board = board_root();

    match cli.cmd {
        Cmd::Doctor | Cmd::Layout => {
            if matches!(cli.cmd, Cmd::Layout) {
                eprintln!("note: `layout` is deprecated; use `cargo xtask doctor`");
            }
            let report = xpost_build::doctor(&board)?;
            for line in &report.lines {
                println!("{line}");
            }
            if let Some(path) = &report.contract_path {
                println!("wrote contract {}", path.display());
            }
        }
        Cmd::LayoutExport { out } => {
            let out_path = match out {
                Some(p) => p,
                None => board.join("target/layout-export/layout.toml"),
            };
            xpost_build::export_contract(&board, &out_path)?;
            println!("wrote contract {}", out_path.display());
        }
        Cmd::Map => {
            xpost_build::map_print(&board)?;
        }
        Cmd::Gen { out } => {
            let out_dir = match out {
                Some(p) => p,
                None => {
                    let p = board.join("target/xtask-gen");
                    std::fs::create_dir_all(&p)
                        .with_context(|| format!("mkdir {}", p.display()))?;
                    p
                }
            };
            let result = xpost_build::generate(&board, &out_dir)?;
            for w in &result.warnings {
                eprintln!("warning: {w}");
            }
            let contract = out_dir.join("layout.toml");
            xpost_build::export_contract(&board, &contract)?;
            println!("enabled fw_us_*: [{}]", result.enabled_us_cfgs.join(", "));
            println!("iram-linker-script: {}", result.custom_sections_path.display());
            println!("wrote codegen artifacts to {}", out_dir.display());
            println!("wrote contract {}", contract.display());
        }
    }
    Ok(())
}
