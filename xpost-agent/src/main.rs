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

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use xpost_serial::client::Command;

use crate::ops::oneshot;
use crate::state::{open_handler, workspace_root};

/// Default XDP ELF for flash xdp.
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
    /// Flash firmware and/or XDP cold-table images.
    Flash {
        #[command(subcommand)]
        kind: FlashCmd,
    },
    /// Clear XDP cold table or a named map / perf.
    Clear {
        #[command(subcommand)]
        kind: ClearCmd,
    },
    /// Get XDP list, map blob, perf blob, or serial window.
    Get {
        #[command(subcommand)]
        kind: GetCmd,
    },
    /// Enable App XDP.
    Enable {
        #[command(subcommand)]
        kind: EnableCmd,
    },
    /// Disable App XDP.
    Disable {
        #[command(subcommand)]
        kind: DisableCmd,
    },
    /// Update a map entry.
    Update {
        #[command(subcommand)]
        kind: UpdateCmd,
    },
    /// Debug: one App command line.
    Run {
        /// Command text without trailing newline.
        cmd: String,
    },
}

#[derive(Subcommand, Debug)]
enum FlashCmd {
    /// Build + flash firmware profile (does not install xdp.elf).
    Fw {
        #[arg(long)]
        profile_id: String,
    },
    /// Write XDP ELF into the cold table.
    Xdp {
        #[arg(long, default_value = DEFAULT_XDP_ELF)]
        elf: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum ClearCmd {
    /// Clear cold-table XDP image(s). `name=*` clears all.
    Xdp {
        #[arg(long)]
        name: String,
    },
    /// App ClearMap.
    Map {
        map_name: String,
    },
    /// App ClearPerf.
    Perf,
}

#[derive(Subcommand, Debug)]
enum GetCmd {
    /// List cold-table XDP images.
    Xdp,
    /// App GetMap → `OK get map <n> b64=…`.
    Map {
        map_name: String,
    },
    /// App GetPerf → `OK get perf <n> b64=…`.
    Perf,
    /// Transcript lines in a host unix-ms window.
    Serial {
        #[arg(long)]
        since_unix_ms: u64,
        #[arg(long)]
        until_unix_ms: u64,
    },
}

#[derive(Subcommand, Debug)]
enum EnableCmd {
    Xdp,
}

#[derive(Subcommand, Debug)]
enum DisableCmd {
    Xdp,
}

#[derive(Subcommand, Debug)]
enum UpdateCmd {
    Map {
        map_name: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        value: String,
    },
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

    let handler = open_handler(cli.port.as_deref(), &cli.log)?;
    let result = match cmd {
        Cmd::Flash { kind } => match kind {
            FlashCmd::Fw { profile_id } => {
                let root = workspace_root();
                oneshot::flash_fw_profile(&handler, &root, &profile_id)
            }
            FlashCmd::Xdp { elf } => oneshot::flash_xdp(&handler, &elf),
        },
        Cmd::Clear { kind } => match kind {
            ClearCmd::Xdp { name } => {
                let name = name.trim();
                if name.is_empty() {
                    bail!("missing --name");
                }
                let names = if name == "*" {
                    Vec::new()
                } else {
                    vec![name.to_string()]
                };
                oneshot::clear_xdp(&handler, names)
            }
            ClearCmd::Map { map_name } => {
                oneshot::app(&handler, &Command::ClearMap { name: &map_name })
            }
            ClearCmd::Perf => oneshot::app(&handler, &Command::ClearPerf),
        },
        Cmd::Get { kind } => match kind {
            GetCmd::Xdp => oneshot::get_xdp(&handler),
            GetCmd::Map { map_name } => {
                oneshot::app(&handler, &Command::GetMap { name: &map_name })
            }
            GetCmd::Perf => oneshot::app(&handler, &Command::GetPerf),
            GetCmd::Serial {
                since_unix_ms,
                until_unix_ms,
            } => oneshot::get_serial(&handler, since_unix_ms, until_unix_ms),
        },
        Cmd::Enable { kind: EnableCmd::Xdp } => {
            oneshot::app(&handler, &Command::EnableXdp)
        }
        Cmd::Disable {
            kind: DisableCmd::Xdp,
        } => oneshot::app(&handler, &Command::DisableXdp),
        Cmd::Update {
            kind: UpdateCmd::Map {
                map_name,
                key,
                value,
            },
        } => oneshot::app(
            &handler,
            &Command::UpdateMap {
                name: &map_name,
                key: key.as_bytes(),
                value: value.as_bytes(),
            },
        ),
        Cmd::Run { cmd } => oneshot::run_line(&handler, &cmd),
    };
    let msg = result?;
    println!("{msg}");
    handler.shutdown();
    Ok(())
}
