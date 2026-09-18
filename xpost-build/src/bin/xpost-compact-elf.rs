//! Host CLI: build Compact ELF V0 (`<stem>.elf`) from AOT image + bpf.toml.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;

use xpost_build::metadata_from_bpf_toml;
use xpost_elf::build_container;

#[derive(Parser, Debug)]
#[command(name = "xpost-compact-elf", about = "Encode XPOST Compact ELF V0")]
struct Args {
    /// Raw AOT image (`esp32c3.bin`).
    #[arg(long)]
    image: PathBuf,
    /// Map metadata from libbpf-mcu `bpf.toml` (with `aot_addr` / `map_vma`).
    #[arg(long)]
    bpf_toml: PathBuf,
    /// XDP program name stored in `.xpost.meta` (also default basename of `-o`).
    #[arg(long)]
    name: String,
    /// Hot-slot load address (from firmware `.xpost.mem` `slot_addr`).
    #[arg(long, value_parser = parse_u32)]
    slot_address: u32,
    /// Helper dispatcher address (from firmware `.xpost.mem` `helper_addr`).
    #[arg(long, value_parser = parse_u32)]
    helper_address: u32,
    /// XPOST machine code (1 = ESP32-C3).
    #[arg(long, default_value_t = 1, value_parser = parse_u32_as_u8)]
    machine: u8,
    /// Reject if `image.len() > max_image_len` (slot length from XPM1).
    #[arg(long, value_parser = parse_u32)]
    max_image_len: Option<u32>,
    /// Output Compact ELF path.
    #[arg(short = 'o', long)]
    out: PathBuf,
}

fn parse_u32(s: &str) -> Result<u32, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        t.parse::<u32>().map_err(|e| e.to_string())
    }
}

fn parse_u32_as_u8(s: &str) -> Result<u8, String> {
    let v = parse_u32(s)?;
    u8::try_from(v).map_err(|_| format!("machine out of u8 range: {v}"))
}

fn run(args: Args) -> Result<()> {
    if args.name.is_empty() || args.name.len() > 31 || !args.name.is_ascii() {
        bail!("--name must be 1..31 ASCII bytes");
    }
    let image = fs::read(&args.image).with_context(|| format!("read {}", args.image.display()))?;
    let meta = metadata_from_bpf_toml(
        &args.bpf_toml,
        &args.name,
        args.slot_address,
        args.helper_address,
        args.machine,
    )?;
    let blob = build_container(&image, &meta, args.max_image_len)
        .map_err(|e| anyhow::anyhow!("build_container: {e:?}"))?;
    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::write(&args.out, blob.as_slice()).with_context(|| format!("write {}", args.out.display()))?;
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xpost-compact-elf: {e:#}");
            ExitCode::FAILURE
        }
    }
}
