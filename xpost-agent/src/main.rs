//! CLI front-end for [`xpost_serial`]. One-shot commands or a long-running HTTP/MCP server.
//!
//! # HTTP / MCP control-plane contract (frozen)
//!
//! **Frozen source:** `~/.cursor/plans/agent_map_xdp_ops_f4bae6b6.plan.md`
//! (section from intro through **MCP / CLI**; marked `FROZEN ABOVE`).
//! That plan is the API shape; this rustdoc restates it for implementors.
//! App wire verbs are [`xpost_serial::command::Command`] /
//! [`xpost_serial::reply::OkReply`] (current crate — plan’s older multiline
//! `get map` text is superseded by header+blob; HTTP JSON below is unchanged).
//!
//! Listen: `cargo xtask agent --listen 0.0.0.0:9240 --no-monitor`  
//! Same port: `/health` + `/v1/*` + Streamable HTTP `/mcp`. No auth/TLS.
//! Long ops ~1200s. USB under `spawn_blocking` + `Mutex` on
//! [`xpost_serial::SerialHandler`]. After open: one
//! [`GetInit`](xpost_serial::command::Command::GetInit) before App routes.
//! Agent is **USB-only**; stim/probe is **not** an agent API (`xpost-test` sends).
//!
//! **Delete** legacy HTTP (no aliases): `flash_profile`, `program`, `clear`,
//! `list`, `perf_clear`, `counters`, `request`, `serial_window`, `probe*`,
//! `/v1/map/{name}`, `/v1/xdp_*`, …
//!
//! ## Envelope
//!
//! Default: `{ "ok": bool, "message": string }`. Business/DUT failure → HTTP 200
//! + `ok:false`. Map decode failure → **502**. MCP tools = same ops, no probe.
//!
//! ## Route table (frozen)
//!
//! | Method | Path | Body | Behavior |
//! |--------|------|------|----------|
//! | GET | `/health` | — | liveness |
//! | GET | `/v1/status` | — | `idle` |
//! | GET | `/v1/dut_status` | — | UART `ready` / `booting` (settle; uses `ping`) |
//! | POST | `/v1/flash/fw` | `{"profile_id"}` | build+flash firmware; **does not** install `xdp.elf` |
//! | POST | `/v1/flash/xdp` | `{"elf"}` | write XDP image into cold table |
//! | POST | `/v1/clear/xdp` | `{"name": "*"\|"prog"\|["a","b"]}` | remove image(s); `*`=all; missing name → err |
//! | GET | `/v1/get/xdp` | — | list installed XDP images |
//! | POST | `/v1/enable/xdp` | `{}` | App `EnableXdp` |
//! | POST | `/v1/disable/xdp` | `{}` | App `DisableXdp` |
//! | POST | `/v1/clear/map/{map_name}` | `{}` | App `ClearMap` |
//! | GET | `/v1/get/map/{map_name}` | — | App `GetMap` → **map JSON** |
//! | POST | `/v1/update/map/{map_name}` | see below | App `UpdateMap` (single or batch) |
//! | POST | `/v1/clear/perf` | `{}` | App `ClearPerf` |
//! | GET | `/v1/get/perf` | — | App `GetPerf` → b64 in `message` (or rich JSON) |
//! | POST | `/v1/get/serial` | `{"since_unix_ms","until_unix_ms"}` | transcript time window |
//! | POST | `/v1/run` | `{"cmd"}` | one App line, first `OK`/`ERR` only — **not** for get map/perf |
//! | — | `/mcp` | — | tools with the same semantics |
//!
//! Non-App (espflash / cold table only — no UART App line): `flash/fw`,
//! `flash/xdp`, `clear/xdp`, `get/xdp`.
//!
//! ### `clear/xdp` body
//!
//! `name` is a JSON `string` or `string[]`. `"*"` clears the whole cold table.
//!
//! ### `update/map/{map_name}` body
//!
//! - Single: `{"key","value"}` (ASCII tokens; ARRAY slot decimal / HASH IPv4).
//! - Batch: `{"entries":[{"key","value"},…]}` in order; stop on first error.
//! - Success message may include `n=<N>` updated count.
//!
//! ## Map JSON (`GET /v1/get/map/{map_name}`)
//!
//! Frozen shape: `{"ok","name","type","entries":[{"key","value"}]}` where `key`
//! and `value` are **strings** (slot decimal or dotted IPv4). Optional
//! `size` / `cap` when known from the blob header. Agent decodes
//! [`OkReply::GetMap`](xpost_serial::reply::OkReply::GetMap) blob → this JSON.
//!
//! ```json
//! {
//!   "ok": true,
//!   "name": "counter",
//!   "type": "array",
//!   "entries": [{ "key": "0", "value": "42" }]
//! }
//! ```
//!
//! ```json
//! {
//!   "ok": true,
//!   "name": "blacklist",
//!   "type": "hash",
//!   "entries": [{ "key": "192.168.1.10", "value": "1" }]
//! }
//! ```
//!
//! ## Perf (`GET /v1/get/perf`)
//!
//! Wire: [`OkReply::GetPerf`](xpost_serial::reply::OkReply::GetPerf) blob.
//! HTTP: success carries length + standard base64 (frozen plan:
//! `message` like `OK get perf <n> b64=…`, or equivalent structured fields
//! with the same payload). Clients decode with the existing report layout.
//!
//! ## MCP / CLI names (frozen)
//!
//! `flash_fw`, `flash_xdp`, `clear_xdp`, `get_xdp`, `enable_xdp`,
//! `disable_xdp`, `clear_map`, `get_map`, `update_map`, `clear_perf`,
//! `get_perf`, `get_serial`, `run`, `get_status` / `dut_status`. **No probe.**
//!
//! ## Serial mapping (current `xpost-serial`)
//!
//! | HTTP | Host action |
//! |------|-------------|
//! | `flash/fw`, `flash/xdp`, `clear/xdp`, `get/xdp` | `SerialHandler` flash / cold-table API |
//! | `enable/xdp` / `disable/xdp` | `Command::EnableXdp` / `DisableXdp` |
//! | `clear/map/{name}` | `Command::ClearMap { name }` |
//! | `get/map/{name}` | `Command::GetMap { name }` |
//! | `update/map/{name}` | `Command::UpdateMap { name, key, value }` |
//! | `clear/perf` / `get/perf` | `Command::ClearPerf` / `GetPerf` |
//! | (startup) | `Command::GetInit` (host-only) |
//! | `run` | parse line → `Command` (debug only) |
//!
//! Flat ERR domains (`ServerError` / `MapError` / `PerfError` / `SyntaxError`)
//! surface in `message` / MCP errors.
//!
//! ## Client notes (`xpost-test`)
//!
//! - Config `host:port` → client prefixes `http://`.
//! - Formal path: typed `/v1/flash/*`, `/v1/clear/*`, `/v1/get/*`,
//!   `/v1/enable|disable/xdp`, `/v1/update/map/*` only — never `/v1/run` for
//!   map/perf framing.
//! - Map seeding via agent serial `update/map`, not UDP MapCrud.

mod http;
mod mcp;
mod ops;
mod state;
mod util;

use std::io::ErrorKind;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use xpost_serial::{
    Chip, FileSink, InstallOutcome, JUST_DROP_TABLE, Session, xdp_name_from_file,
};

/// Default XDP ELF for just_drop cold table.
const DEFAULT_XDP_ELF: &str = "examples/just_xdp/just_drop.elf";
/// Default serial transcript path (tee to stdout).
const DEFAULT_LOG: &str = "target/xpost-agent/serial.log";

#[derive(Parser, Debug)]
#[command(name = "xpost-agent", about = "Exercise xpost-serial against an ESP32-C3")]
struct Cli {
    /// Serial device (e.g. /dev/cu.usbmodem21101). Omit to auto-pick a unique USB port.
    #[arg(long, global = true)]
    port: Option<PathBuf>,

    /// Append serial transcript here (also printed to stdout).
    #[arg(long, global = true, default_value = DEFAULT_LOG)]
    log: PathBuf,

    /// Bind HTTP server (`/health`, `/v1/*`, `/mcp`). Mutually exclusive with subcommands.
    #[arg(long)]
    listen: Option<String>,

    /// Accepted for fwctl compatibility (agent owns the USB session; no extra monitor).
    #[arg(long, default_value_t = false)]
    no_monitor: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
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
    /// Clear DUT perf accumulators; await `OK perf clear`.
    PerfClear {
        #[arg(long, default_value_t = 5.0)]
        timeout_secs: f64,
    },
    /// Send one UDP stimulus to the firmware and wait for `ACK`.
    ///
    /// Timeout prints `no-ack` (expected when drop XDP is loaded).
    Probe {
        /// Firmware IPv4 address.
        esp: String,
        /// Firmware UDP port (just_drop stim port).
        #[arg(long, default_value_t = 8080)]
        port: u16,
        #[arg(long, default_value_t = 2.0)]
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

fn open_session(port: Option<&Path>, log: &Path) -> Result<Session<FileSink>> {
    let sink = FileSink::create(log, true)
        .with_context(|| format!("create serial log {}", log.display()))?;
    eprintln!("xpost-agent serial log {}", sink.path().display());
    Session::open(port, Chip::Esp32c3, sink).context("open session")
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let _ = cli.no_monitor;
    if let Some(addr) = cli.listen.as_deref() {
        if cli.cmd.is_some() {
            bail!("--listen cannot be combined with a subcommand");
        }
        return http::serve_http(addr, cli.port.as_deref(), &cli.log);
    }
    let Some(cmd) = cli.cmd else {
        bail!("subcommand required (or pass --listen ADDR)");
    };
    match cmd {
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
            let mut session = open_session(cli.port.as_deref(), &cli.log)?;
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
            let mut session = open_session(cli.port.as_deref(), &cli.log)?;
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
            let mut session = open_session(cli.port.as_deref(), &cli.log)?;
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
            let mut session = open_session(cli.port.as_deref(), &cli.log)?;
            let reply = session
                .request_timeout(&cmd, Duration::from_secs_f64(timeout_secs))
                .with_context(|| format!("request `{cmd}`"))?;
            println!("{reply}");
            session.close().context("close")?;
            Ok(())
        }
        Cmd::PerfClear { timeout_secs } => {
            let mut session = open_session(cli.port.as_deref(), &cli.log)?;
            let reply = session
                .request_timeout("perf clear", Duration::from_secs_f64(timeout_secs))
                .context("request `perf clear`")?;
            if !reply.trim().eq_ignore_ascii_case("OK perf clear")
                && !reply
                    .trim()
                    .to_ascii_lowercase()
                    .starts_with("ok perf clear")
            {
                bail!("unexpected perf clear reply: {reply}");
            }
            println!("{reply}");
            session.close().context("close")?;
            Ok(())
        }
        Cmd::Probe {
            esp,
            port,
            timeout_secs,
        } => {
            let got = probe_ack(&esp, port, Duration::from_secs_f64(timeout_secs))?;
            println!("{got}");
            Ok(())
        }
    }
}

/// One datagram to the DUT. Returns `ack` or `no-ack`.
pub(crate) fn probe_ack(esp: &str, port: u16, timeout: Duration) -> Result<&'static str> {
    let sock = UdpSocket::bind("0.0.0.0:0").context("bind probe socket")?;
    sock.set_read_timeout(Some(timeout))
        .context("set probe timeout")?;
    let dest = format!("{esp}:{port}");
    sock.send_to(b"STIM\n", &dest)
        .with_context(|| format!("send stim to {dest}"))?;
    let mut buf = [0u8; 64];
    match sock.recv_from(&mut buf) {
        Ok((n, from)) => {
            let msg = String::from_utf8_lossy(&buf[..n]);
            let msg = msg.trim_end();
            if msg.starts_with("ACK") {
                Ok("ack")
            } else {
                bail!("unexpected payload from {from}: {msg}");
            }
        }
        Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {
            Ok("no-ack")
        }
        Err(e) => Err(e).context("recv ack"),
    }
}
