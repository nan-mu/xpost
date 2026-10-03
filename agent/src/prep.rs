//! Before serving RPC: build profile firmware + doctor/check.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::profiles::Profile;

/// Example name under `xpost/examples/` derived from profile bin path.
fn example_for_profile(root: &Path, profile: &Profile) -> Result<String> {
    let examples = root.join("examples");
    let bin = &profile.bin;
    let rel = bin
        .strip_prefix(&examples)
        .with_context(|| format!("profile `{}` bin not under examples/", profile.id))?;
    let example = rel
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .context("profile bin path has no example segment")?;
    Ok(example.to_string())
}

fn rustup_bin() -> Option<PathBuf> {
    if let Ok(path) = Command::new("rustup").arg("--version").output() {
        if path.status.success() {
            return Some(PathBuf::from("rustup"));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let rustup = PathBuf::from(home).join(".cargo/bin/rustup");
        if rustup.is_file() {
            return Some(rustup);
        }
    }
    None
}

/// Resolve `cargo` via rustup so `rust-toolchain.toml` wins over MacPorts `/opt/local/bin/cargo`.
fn toolchain_cargo(dir: &Path) -> PathBuf {
    if let Some(rustup) = rustup_bin() {
        if let Ok(out) = Command::new(&rustup)
            .current_dir(dir)
            .args(["which", "cargo"])
            .output()
        {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
        }
    }
    PathBuf::from("cargo")
}

fn toolchain_rustc(cargo: &Path) -> Option<PathBuf> {
    let rustc = cargo.with_file_name("rustc");
    rustc.is_file().then_some(rustc)
}

fn cargo_cmd(dir: &Path) -> Command {
    let cargo = toolchain_cargo(dir);
    let mut cmd = Command::new(&cargo);
    if let Some(bin) = cargo.parent() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        let path = format!("{}{}{}", bin.display(), sep, path.to_string_lossy());
        cmd.env("PATH", path);
    }
    if let Some(rustc) = toolchain_rustc(&cargo) {
        cmd.env("RUSTC", rustc);
    }
    cmd
}

fn run_cargo_xtask(root: &Path, rest: &[&str]) -> Result<()> {
    let mut cmd = cargo_cmd(root);
    cmd.current_dir(root).arg("xtask");
    for a in rest {
        cmd.arg(a);
    }
    let status = cmd
        .status()
        .with_context(|| format!("cargo xtask {}", rest.join(" ")))?;
    if !status.success() {
        bail!("cargo xtask {} failed", rest.join(" "));
    }
    Ok(())
}

fn check_wifi_config(example_dir: &Path) -> Result<()> {
    const PLACEHOLDER: &str = "your_wifi_ssid";
    let cfg = example_dir.join("config.toml");
    if cfg.is_file() {
        let raw = std::fs::read_to_string(&cfg).context("read config.toml")?;
        if raw.contains(PLACEHOLDER) {
            bail!(
                "{} still has placeholder WiFi (`{PLACEHOLDER}`); set [wifi].ssid/password before agent prep",
                cfg.display()
            );
        }
    }
    let fw_cfg = example_dir.join("src/fw_cfg.rs");
    if fw_cfg.is_file() {
        let raw = std::fs::read_to_string(&fw_cfg).context("read fw_cfg.rs")?;
        if raw.contains(PLACEHOLDER) {
            bail!(
                "{} still has placeholder WiFi (`{PLACEHOLDER}`); edit WiFi constants before agent prep",
                fw_cfg.display()
            );
        }
    }
    Ok(())
}

fn prep_example(root: &Path, example: &str, profiles: &[Profile]) -> Result<()> {
    let desc = profiles
        .iter()
        .find(|p| p.bin.components().any(|c| c.as_os_str() == example))
        .map(|p| p.description.as_str())
        .unwrap_or(example);
    eprintln!("xpost-agent prep: examples/{example} — {desc}");

    let example_dir = root.join("examples").join(example);
    check_wifi_config(&example_dir)?;
    if example_dir.join("layout.toml").is_file() {
        run_cargo_xtask(root, &[example, "doctor"])?;
    } else {
        eprintln!("xpost-agent prep: skip doctor (no layout.toml) for {example}");
    }
    run_cargo_xtask(root, &[example, "check"])?;
    run_cargo_xtask(root, &[example, "build", "--release"])?;

    Ok(())
}

/// Build every example referenced by profiles; firmware also runs `doctor`.
pub fn prepare_profiles(root: &Path, profiles: &[Profile]) -> Result<()> {
    let mut examples: Vec<String> = Vec::new();
    for p in profiles {
        let ex = example_for_profile(root, p)?;
        if !examples.iter().any(|e| e == &ex) {
            examples.push(ex);
        }
    }
    for ex in &examples {
        prep_example(root, ex, profiles)?;
    }

    for p in profiles {
        if !p.bin.is_file() {
            bail!(
                "profile `{}` bin missing after build: {}",
                p.id,
                p.bin.display()
            );
        }
    }
    Ok(())
}
