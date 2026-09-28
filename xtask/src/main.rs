//! Root dispatcher: `cargo xtask sync …` or `cargo xtask <example> <cmd…>`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "xpost workspace: sync (xpost-build) or dispatch into examples/<name>"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<RootCmd>,
    /// Legacy: `cargo xtask <example> <cmd…>` when no subcommand matches.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    rest: Vec<String>,
}

#[derive(Subcommand)]
enum RootCmd {
    /// Call xpost-build sync APIs (does not modify examples/).
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
}

#[derive(Subcommand)]
enum SyncAction {
    Validate {
        #[arg(long)]
        board: Option<PathBuf>,
    },
    Codegen {
        #[arg(long)]
        board: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    Export {
        #[arg(long)]
        board: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    PatchElf {
        #[arg(long)]
        board: PathBuf,
        #[arg(long)]
        elf: PathBuf,
        #[arg(long)]
        contract: Option<PathBuf>,
    },
    /// Embed XPM1 `.xpost.mem` into a linked firmware ELF (path A endpoint).
    EmbedLayout {
        #[arg(long)]
        board: PathBuf,
        #[arg(long)]
        elf: PathBuf,
    },
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask parent")
        .to_path_buf()
}

fn host_triple() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else {
        "aarch64-apple-darwin"
    }
}

fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let root = workspace_root();

    if let Some(RootCmd::Sync { action }) = cli.cmd {
        return run_sync(&root, action);
    }

    // Legacy dispatcher: first trailing arg is example name.
    let mut args = cli.rest;
    if args.is_empty() {
        bail!("usage: cargo xtask sync <…> | cargo xtask <example> <cmd…>");
    }
    let example = args.remove(0);
    dispatch_example(&root, &example, &args)
}

fn run_sync(root: &Path, action: SyncAction) -> Result<ExitCode> {
    let crate_manifest = root.join("Cargo.toml");
    let tmp_out = root.join("target/xpost-sync");
    match action {
        SyncAction::Validate { board } => {
            let out = tmp_out.clone();
            let req = xpost_build::SyncRequest {
                crate_manifest: &crate_manifest,
                board_dir: board.as_deref(),
                out_dir: &out,
                contract_out: None,
                elf_path: None,
            };
            let report = xpost_build::validate(&req)?;
            for line in report.lines {
                println!("{line}");
            }
            if let Some(board) = board.as_ref()
                && let Some(table) = report.meta.xdp_table.as_ref()
            {
                let csv = board.join("partitions.csv");
                if csv.is_file() {
                    xpost_build::validate_xdp_table_against_partitions(table, &csv)?;
                }
            }
        }
        SyncAction::Codegen { board, out } => {
            let req = xpost_build::SyncRequest {
                crate_manifest: &crate_manifest,
                board_dir: board.as_deref(),
                out_dir: &out,
                contract_out: None,
                elf_path: None,
            };
            let report = xpost_build::codegen(&req)?;
            if let Some(p) = report.custom_sections_path {
                println!("custom-sections={}", p.display());
            }
        }
        SyncAction::Export { board, out } => {
            let contract = out.unwrap_or_else(|| board.join("target/layout-export/layout.toml"));
            let out_dir = contract
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| board.join("target/layout-export"));
            let req = xpost_build::SyncRequest {
                crate_manifest: &crate_manifest,
                board_dir: Some(&board),
                out_dir: &out_dir,
                contract_out: Some(&contract),
                elf_path: None,
            };
            let report = xpost_build::export(&req)?;
            if let Some(p) = report.contract_path {
                println!("contract={}", p.display());
            }
        }
        SyncAction::PatchElf {
            board,
            elf,
            contract,
        } => {
            let contract =
                contract.unwrap_or_else(|| board.join("target/layout-export/layout.toml"));
            let out_dir = contract
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| board.join("target/layout-export"));
            let req = xpost_build::SyncRequest {
                crate_manifest: &crate_manifest,
                board_dir: Some(&board),
                out_dir: &out_dir,
                contract_out: Some(&contract),
                elf_path: Some(&elf),
            };
            xpost_build::patch_elf(&req)?;
            println!("patched {}", contract.display());
        }
        SyncAction::EmbedLayout { board, elf } => {
            let out = tmp_out.clone();
            let req = xpost_build::SyncRequest {
                crate_manifest: &crate_manifest,
                board_dir: Some(&board),
                out_dir: &out,
                contract_out: None,
                elf_path: Some(&elf),
            };
            let report = xpost_build::embed_layout_sync(&req)?;
            for line in report.lines {
                println!("{line}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn dispatch_example(root: &PathBuf, example: &str, rest: &[String]) -> Result<ExitCode> {
    if example == "agent" {
        // `xpost-agent/` is workspace-exclude (own host crate). Do not use `-p`
        // on the RV32 root workspace. CLI stays `cargo xtask agent …`.
        let manifest = root.join("xpost-agent/Cargo.toml");
        let mut args = vec![
            "run".to_string(),
            "--manifest-path".to_string(),
            manifest.display().to_string(),
            "--target".to_string(),
            host_triple().to_string(),
            "--".to_string(),
        ];
        args.extend(rest.iter().cloned());
        let status = Command::new("cargo")
            .current_dir(root)
            .args(&args)
            .status()
            .context("xpost-agent")?;
        return Ok(ExitCode::from(status.code().unwrap_or(1) as u8));
    }

    let example_dir = root.join("examples").join(example);
    if !example_dir.is_dir() {
        bail!(
            "unknown example `{example}` (expected {})",
            example_dir.display()
        );
    }

    let xtask_manifest = example_dir.join("xtask/Cargo.toml");
    let status = if rest.first().is_some_and(|a| {
        matches!(
            a.as_str(),
            "build" | "check" | "run" | "clean" | "clippy" | "fmt"
        )
    }) {
        Command::new("cargo")
            .current_dir(&example_dir)
            .env("CARGO_TARGET_DIR", example_dir.join("target"))
            .env("XPOST_BOARD", &example_dir)
            .args(rest)
            .status()
            .with_context(|| format!("cargo in {}", example_dir.display()))?
    } else if xtask_manifest.is_file() {
        let mut args = vec![
            "run".into(),
            "--manifest-path".into(),
            xtask_manifest.display().to_string(),
            "--features".into(),
            "cli".into(),
            "--target".into(),
            host_triple().into(),
            "--".into(),
        ];
        args.extend(rest.iter().cloned());
        Command::new("cargo")
            .current_dir(root)
            .args(&args)
            .status()
            .with_context(|| format!("{example} xtask"))?
    } else {
        bail!("example `{example}` has no xtask mapping yet");
    };

    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}
