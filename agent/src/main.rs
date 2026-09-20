//! CLI front-end for [`xpost_serial`]. True hardware verification entry.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use xpost_serial::{Chip, Image, Session, StdoutSink, XdpImage};

/// Default Compact ELF used with just_drop cold table (plan lock-in).
const DEFAULT_XDP_ELF: &str = "/Users/nan/bs/build/xdp_paper/xdp_paper.elf";
const DEFAULT_XDP_ADDR: &str = "0x310000";

fn parse_u32_arg(s: &str) -> std::result::Result<u32, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        t.parse().map_err(|e| format!("{e}"))
    }
}

#[derive(Parser, Debug)]
#[command(name = "xpost-agent", about = "Exercise xpost-serial against an ESP32-C3")]
struct Cli {
    /// Serial device (e.g. /dev/ttyUSB0). Omit to auto-pick a single USB port.
    #[arg(long, global = true)]
    port: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Write Compact ELF to cold flash, read back, byte-compare.
    WriteBin {
        /// Absolute flash address (just_drop TABLE.0 = 0x310000).
        #[arg(long, default_value = DEFAULT_XDP_ADDR, value_parser = parse_u32_arg)]
        addr: u32,
        /// Path to Compact ELF (whole file).
        #[arg(long, default_value = DEFAULT_XDP_ELF)]
        elf: PathBuf,
        /// Soft capacity gate (default 0x1000).
        #[arg(long, default_value = "0x1000", value_parser = parse_usize_arg)]
        capacity: usize,
    },
    /// Flash firmware app (+ optional verified xdp write-bin), then reset to App.
    Program {
        #[arg(long)]
        firmware: PathBuf,
        #[arg(long)]
        partitions: PathBuf,
        #[arg(long)]
        xdp_elf: Option<PathBuf>,
        #[arg(long, default_value = DEFAULT_XDP_ADDR, value_parser = parse_u32_arg)]
        xdp_addr: u32,
        #[arg(long, default_value = "0x1000", value_parser = parse_usize_arg)]
        capacity: usize,
    },
    /// Send one App-mode line; print first OK/ERR reply.
    Request {
        /// Command text without trailing newline.
        cmd: String,
        #[arg(long, default_value_t = 5.0)]
        timeout_secs: f64,
    },
}

fn parse_usize_arg(s: &str) -> std::result::Result<usize, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        t.parse().map_err(|e| format!("{e}"))
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xpost-agent: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::WriteBin {
            addr,
            elf,
            capacity,
        } => {
            if !elf.is_file() {
                bail!("elf not found: {}", elf.display());
            }
            let mut session = Session::open(cli.port.as_deref(), Chip::Esp32c3, StdoutSink)
                .context("open session")?;
            session.image_capacity = capacity;
            session
                .write_bin(addr, &elf)
                .with_context(|| format!("write_bin {addr:#x} {}", elf.display()))?;
            session.close().context("close")?;
            Ok(())
        }
        Cmd::Program {
            firmware,
            partitions,
            xdp_elf,
            xdp_addr,
            capacity,
        } => {
            if !firmware.is_file() {
                bail!("firmware not found: {}", firmware.display());
            }
            if !partitions.is_file() {
                bail!("partitions not found: {}", partitions.display());
            }
            let xdp = match xdp_elf.as_ref() {
                Some(p) => {
                    if !p.is_file() {
                        bail!("xdp elf not found: {}", p.display());
                    }
                    Some(XdpImage {
                        addr: xdp_addr,
                        path: p.as_path(),
                    })
                }
                None => None,
            };
            let mut session = Session::open(cli.port.as_deref(), Chip::Esp32c3, StdoutSink)
                .context("open session")?;
            session.image_capacity = capacity;
            let img = Image {
                firmware: firmware.as_path(),
                partitions: partitions.as_path(),
                xdp,
            };
            session.program(&img).context("program")?;
            session.close().context("close")?;
            Ok(())
        }
        Cmd::Request { cmd, timeout_secs } => {
            let mut session = Session::open(cli.port.as_deref(), Chip::Esp32c3, StdoutSink)
                .context("open session")?;
            let reply = session
                .request_timeout(&cmd, Duration::from_secs_f64(timeout_secs))
                .with_context(|| format!("request `{cmd}`"))?;
            println!("{reply}");
            session.close().context("close")?;
            Ok(())
        }
    }
}
