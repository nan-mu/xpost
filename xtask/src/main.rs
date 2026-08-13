//! Optional CLI for firmware codegen (default path is `build.rs` → lib).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "firmware codegen (also invoked from build.rs)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Validate `config.toml` [arena]/[helper]/[slots] and write `mem.toml`.
    Doctor,
    /// Deprecated alias for `doctor`.
    Layout,
    /// Print IBUS/DBUS dual-map SRAM diagram from `mem.toml`.
    Map,
    /// Parse tomls under firmware/ and write generated artifacts to `--out`.
    Gen {
        /// Output directory (defaults to a temp dir under firmware/target/xtask-gen).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run generate into a temp dir (validates tomls without writing durable artifacts).
    Check,
}

fn firmware_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask parent")
        .to_path_buf()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let manifest_dir = firmware_root();

    match cli.cmd {
        Cmd::Doctor | Cmd::Layout => {
            if matches!(cli.cmd, Cmd::Layout) {
                eprintln!("note: `layout` is deprecated; use `cargo xtask doctor`");
            }
            let report = xtask::doctor(&manifest_dir)?;
            for line in &report.lines {
                println!("{line}");
            }
            println!("wrote {}", report.mem_path.display());
        }
        Cmd::Map => {
            xtask::map_print(&manifest_dir)?;
        }
        Cmd::Gen { out } => {
            let out_dir = match out {
                Some(p) => p,
                None => {
                    let p = manifest_dir.join("target/xtask-gen");
                    std::fs::create_dir_all(&p)
                        .with_context(|| format!("mkdir {}", p.display()))?;
                    p
                }
            };
            run_gen(&manifest_dir, &out_dir)?;
            println!("wrote codegen artifacts to {}", out_dir.display());
        }
        Cmd::Check => {
            let tmp = tempfile_dir(&manifest_dir)?;
            run_gen(&manifest_dir, &tmp)?;
            println!("xtask check ok");
        }
    }
    Ok(())
}

fn run_gen(manifest_dir: &PathBuf, out_dir: &PathBuf) -> Result<()> {
    let result = xtask::generate(manifest_dir, out_dir)?;
    for w in &result.warnings {
        eprintln!("warning: {w}");
    }
    println!("enabled fw_us_*: [{}]", result.enabled_us_cfgs.join(", "));
    println!("custom-sections: {}", result.custom_sections_path.display());
    Ok(())
}

fn tempfile_dir(manifest_dir: &PathBuf) -> Result<PathBuf> {
    let base = manifest_dir.join("target/xtask-check");
    if base.exists() {
        std::fs::remove_dir_all(&base).with_context(|| format!("remove {}", base.display()))?;
    }
    std::fs::create_dir_all(&base).with_context(|| format!("mkdir {}", base.display()))?;
    if !base.is_dir() {
        bail!("{} is not a directory", base.display());
    }
    Ok(base)
}
