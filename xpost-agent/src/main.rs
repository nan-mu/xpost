//! CLI front-end for [`xpost_serial`]. True hardware verification entry.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use xpost_serial::{
    Chip, InstallOutcome, JUST_DROP_TABLE, Session, StdoutSink, xdp_name_from_file,
};

/// Default XDP ELF for just_drop cold table.
const DEFAULT_XDP_ELF: &str = "/Users/nan/bs/build/just_drop/just_drop.elf";

#[derive(Parser, Debug)]
#[command(name = "xpost-agent", about = "Exercise xpost-serial against an ESP32-C3")]
struct Cli {
    /// Serial device (e.g. /dev/cu.usbmodem21101). Omit to auto-pick a unique USB port.
    #[arg(long, global = true)]
    port: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Install XDP ELF into the cold table by program name (overwrite or append).
    Program {
        /// Path to XDP ELF (`xdp.elf` / `<stem>.elf`).
        #[arg(long, default_value = DEFAULT_XDP_ELF)]
        elf: PathBuf,
        /// Cold table absolute base (default just_drop `0x310000`).
        #[arg(long, default_value_t = JUST_DROP_TABLE.base, value_parser = parse_u32_arg)]
        table_base: u32,
        /// Cold table length (default just_drop `0x10000`).
        #[arg(long, default_value_t = JUST_DROP_TABLE.len, value_parser = parse_u32_arg)]
        table_len: u32,
        /// Max container size (default `0x1000`).
        #[arg(long, default_value_t = JUST_DROP_TABLE.image_capacity as u32, value_parser = parse_u32_arg)]
        capacity: u32,
    },
    /// List XDP ELF entries currently in the cold table.
    List {
        #[arg(long, default_value_t = JUST_DROP_TABLE.base, value_parser = parse_u32_arg)]
        table_base: u32,
        #[arg(long, default_value_t = JUST_DROP_TABLE.len, value_parser = parse_u32_arg)]
        table_len: u32,
    },
    /// Erase the entire cold table (all XDP ELF containers).
    Clear {
        #[arg(long, default_value_t = JUST_DROP_TABLE.base, value_parser = parse_u32_arg)]
        table_base: u32,
        #[arg(long, default_value_t = JUST_DROP_TABLE.len, value_parser = parse_u32_arg)]
        table_len: u32,
    },
    /// Send one App-mode line; print first OK/ERR reply.
    Request {
        /// Command text without trailing newline.
        cmd: String,
        #[arg(long, default_value_t = 5.0)]
        timeout_secs: f64,
    },
}

fn parse_u32_arg(s: &str) -> std::result::Result<u32, String> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
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
        Cmd::Program {
            elf,
            table_base,
            table_len,
            capacity,
        } => {
            if !elf.is_file() {
                bail!("xdp.elf not found: {}", elf.display());
            }
            let name = xdp_name_from_file(&elf)
                .with_context(|| format!("parse name from {}", elf.display()))?;
            let mut session = Session::open(cli.port.as_deref(), Chip::Esp32c3, StdoutSink)
                .context("open session")?;
            session.cold_table = xpost_serial::ColdTable {
                base: table_base,
                len: table_len,
                image_capacity: capacity as usize,
            };
            session.image_capacity = capacity as usize;
            let outcome = session
                .install_xdp(&name, &elf)
                .with_context(|| format!("install_xdp `{name}` {}", elf.display()))?;
            match outcome {
                InstallOutcome::Overwritten { offset } => {
                    println!("overwritten name={name} table_off={offset:#x}");
                }
                InstallOutcome::Created { offset } => {
                    println!("created name={name} table_off={offset:#x}");
                }
            }
            session.close().context("close")?;
            Ok(())
        }
        Cmd::List {
            table_base,
            table_len,
        } => {
            let mut session = Session::open(cli.port.as_deref(), Chip::Esp32c3, StdoutSink)
                .context("open session")?;
            session.cold_table = xpost_serial::ColdTable {
                base: table_base,
                len: table_len,
                image_capacity: JUST_DROP_TABLE.image_capacity,
            };
            let entries = session.list_xdp().context("list_xdp")?;
            if entries.is_empty() {
                println!("(empty cold table)");
            } else {
                for e in &entries {
                    println!(
                        "name={} offset={:#x} len={:#x}",
                        e.name, e.offset, e.len
                    );
                }
            }
            session.close().context("close")?;
            Ok(())
        }
        Cmd::Clear {
            table_base,
            table_len,
        } => {
            let mut session = Session::open(cli.port.as_deref(), Chip::Esp32c3, StdoutSink)
                .context("open session")?;
            session.cold_table = xpost_serial::ColdTable {
                base: table_base,
                len: table_len,
                image_capacity: JUST_DROP_TABLE.image_capacity,
            };
            session.clear_xdp().context("clear_xdp")?;
            println!(
                "cleared cold table base={table_base:#x} len={table_len:#x}"
            );
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
