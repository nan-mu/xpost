//! Linker wrapper: run `rust-lld`, then embed `.xpost.mem` into the output ELF.
//!
//! Boards point `[target.*.linker]` at `xpost-build/xpost-link` (shell → host
//! bin). [`crate::firmware_codegen`] writes embed meta; the wrapper reads it
//! after a successful link so plain `cargo build` yields XPM1.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::embed;

/// Env set by [`crate::firmware_codegen`] for the rustc → linker child.
pub const EMBED_META_ENV: &str = "XPOST_EMBED_META";

/// If `args` look like a linker invocation, wrap `rust-lld` + embed and return
/// `Some(exit)`. Otherwise return `None` so the build script can continue.
pub fn try_run(args: &[String]) -> Option<ExitCode> {
    if !looks_like_linker(args) {
        return None;
    }
    match run_linker(args) {
        Ok(()) => Some(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("xpost-link: {e:#}");
            Some(ExitCode::FAILURE)
        }
    }
}

/// Entry for the `xpost-link` binary (always treated as a linker wrapper).
pub fn main_linker(args: &[String]) -> ExitCode {
    match run_linker(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xpost-link: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run_linker(args: &[String]) -> Result<()> {
    let lld = find_rust_lld()?;
    let status = Command::new(&lld)
        .args(args)
        .status()
        .with_context(|| format!("spawn {}", lld.display()))?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        bail!("rust-lld failed (exit {code})");
    }

    let Some(elf) = output_path(args) else {
        bail!("linker args missing -o <elf>");
    };
    let meta = embed_meta_from_env()?;
    let map = embed::embed_layout(&meta.crate_manifest, &meta.board_dir, &elf)?;
    eprintln!(
        "xpost-link: embedded {} helper={:#x} slot={:#x}+{:#x} table={:#x}+{:#x} → {}",
        xpost_elf::SECTION_NAME,
        map.helper_addr,
        map.slot_addr,
        map.slot_len,
        map.table_addr,
        map.table_len,
        elf.display()
    );
    Ok(())
}

fn looks_like_linker(args: &[String]) -> bool {
    if args.is_empty() {
        return false;
    }
    // rustc error-handling-script protocol (see esp-hal build.rs helpers).
    if matches!(
        args[0].as_str(),
        "undefined-symbol" | "missing-lib" | "duplicate-symbol"
    ) {
        return false;
    }
    args.iter()
        .any(|a| a == "-o" || a.starts_with("-o") || a.starts_with("--output"))
}

struct EmbedMeta {
    crate_manifest: PathBuf,
    board_dir: PathBuf,
}

fn embed_meta_from_env() -> Result<EmbedMeta> {
    if let Some(path) = env::var_os(EMBED_META_ENV).map(PathBuf::from) {
        return parse_embed_meta(&path);
    }
    // Fallback when rustc did not forward cargo:rustc-env to the linker.
    if let Ok(manifest) = env::var("CARGO_MANIFEST_DIR") {
        let stable = PathBuf::from(manifest).join("target/xpost_embed_meta.txt");
        if stable.is_file() {
            return parse_embed_meta(&stable);
        }
    }
    bail!("{EMBED_META_ENV} not set and no target/xpost_embed_meta.txt (firmware_codegen missing?)")
}

/// Write / parse a two-line meta file used across the rustc → linker boundary.
pub fn write_embed_meta(path: &Path, crate_manifest: &Path, board_dir: &Path) -> Result<()> {
    let body = format!(
        "crate_manifest={}\nboard_dir={}\n",
        crate_manifest.display(),
        board_dir.display()
    );
    fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn parse_embed_meta(path: &Path) -> Result<EmbedMeta> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut crate_manifest = None;
    let mut board_dir = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("crate_manifest=") {
            crate_manifest = Some(PathBuf::from(v));
        } else if let Some(v) = line.strip_prefix("board_dir=") {
            board_dir = Some(PathBuf::from(v));
        }
    }
    Ok(EmbedMeta {
        crate_manifest: crate_manifest
            .ok_or_else(|| anyhow::anyhow!("{}: missing crate_manifest", path.display()))?,
        board_dir: board_dir.ok_or_else(|| anyhow::anyhow!("{}: missing board_dir", path.display()))?,
    })
}

fn output_path(args: &[String]) -> Option<PathBuf> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-o" || a == "--output" {
            return args.get(i + 1).map(PathBuf::from);
        }
        if let Some(rest) = a.strip_prefix("-o") {
            if !rest.is_empty() {
                return Some(PathBuf::from(rest));
            }
        }
        if let Some(rest) = a.strip_prefix("--output=") {
            return Some(PathBuf::from(rest));
        }
        i += 1;
    }
    None
}

fn find_rust_lld() -> Result<PathBuf> {
    if let Ok(p) = env::var("XPOST_RUST_LLD") {
        return Ok(PathBuf::from(p));
    }
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let sysroot = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .with_context(|| format!("run {rustc} --print sysroot"))?;
    if !sysroot.status.success() {
        bail!("{rustc} --print sysroot failed");
    }
    let sysroot = String::from_utf8(sysroot.stdout)?.trim().to_string();

    let host = Command::new(&rustc)
        .arg("-vV")
        .output()
        .with_context(|| format!("run {rustc} -vV"))?;
    let host_line = String::from_utf8(host.stdout)?;
    let host = host_line
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or_else(|| anyhow::anyhow!("rustc -vV: no host: line"))?;

    let lld = PathBuf::from(&sysroot)
        .join("lib/rustlib")
        .join(host)
        .join("bin/rust-lld");
    if !lld.is_file() {
        bail!("rust-lld not found at {}", lld.display());
    }
    Ok(lld)
}
