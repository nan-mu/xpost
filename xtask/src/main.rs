//! Root dispatcher: `cargo xtask <example> <cmd…>` → example package / board xtask.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use clap::Parser;

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "Dispatch into examples/<name> (independent MCU projects)"
)]
struct Cli {
    /// Example directory under examples/ (e.g. firmware)
    example: String,
    /// Args forwarded to that example's xtask, or cargo build/check/run/…
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    rest: Vec<String>,
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

    if cli.example == "agent" {
        let mut args = vec![
            "run".to_string(),
            "-p".to_string(),
            "xpost-agent".to_string(),
            "--".to_string(),
        ];
        args.extend(cli.rest);
        let status = Command::new("cargo")
            .current_dir(&root)
            .args(&args)
            .status()
            .context("xpost-agent")?;
        return Ok(ExitCode::from(status.code().unwrap_or(1) as u8));
    }

    let example_dir = root.join("examples").join(&cli.example);
    if !example_dir.is_dir() {
        bail!(
            "unknown example `{}` (expected {})",
            cli.example,
            example_dir.display()
        );
    }

    let xtask_manifest = example_dir.join("xtask/Cargo.toml");
    let status = if cli.rest.first().is_some_and(|a| {
        matches!(
            a.as_str(),
            "build" | "check" | "run" | "clean" | "clippy" | "fmt"
        )
    }) {
        // MCU builds: run inside the example so its .cargo/config (build-std, runner) applies.
        // Pin target-dir under the example so workspace-member firmware matches profiles.toml bins.
        // Force board dir: workspace-root .cargo/config sets XPOST_BOARD=examples/firmware and
        // would otherwise codegen the wrong bpf.toml into path-dep `xpost`.
        Command::new("cargo")
            .current_dir(&example_dir)
            .env("CARGO_TARGET_DIR", example_dir.join("target"))
            .env("XPOST_BOARD", &example_dir)
            .args(&cli.rest)
            .status()
            .with_context(|| format!("cargo in {}", example_dir.display()))?
    } else if xtask_manifest.is_file() {
        // Host board xtask (doctor/map/gen): manifest-path keeps us at workspace root.
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
        args.extend(cli.rest.iter().cloned());
        Command::new("cargo")
            .current_dir(&root)
            .args(&args)
            .status()
            .with_context(|| format!("{} xtask", cli.example))?
    } else {
        bail!(
            "example `{}` has no xtask mapping yet; cd examples/{} and run locally",
            cli.example,
            cli.example
        );
    };

    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}
