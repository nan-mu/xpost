//! Long-running HTTP/MCP daemon front-end for [`xpost_serial`].
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
//! Run (daemon, cwd = `xpost-agent/`):
//! ```text
//! cargo run --release -- --bind 0.0.0.0:9240
//! cargo run --release -- --bind 0.0.0.0:9240 --log-dir target/xpost-agent
//! ```
//! Same port: `/health` + `/v1/*` + Streamable HTTP `/mcp`. No auth/TLS.
//! Long ops ~1200s. USB under `spawn_blocking` + `Mutex` on
//! [`xpost_serial::SerialHandler`]. Startup only opens the shared USB session;
//! App/flash work runs on request.
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
//! | GET | `/v1/dut_status` | — | UART `ready` / `booting` (uses `ping`) |
//! | GET | `/v1/get/init` | — | host wait for unsolicited `OK init` |
//! | POST | `/v1/flash/fw` | `{"name"}` | build+flash firmware, then co-flash `fw.xdp[]` |
//! | POST | `/v1/flash/xdp` | `{"name"}` | build+write named XDP image into cold table |
//! | POST | `/v1/clear/xdp` | `{"name": "*"\|"prog"\|["a","b"]}` | remove image(s); `*`=all; missing name → err |
//! | GET | `/v1/get/xdp` | — | list installed XDP images |
//! | POST | `/v1/enable/xdp` | `{}` | App `EnableXdp` |
//! | POST | `/v1/disable/xdp` | `{}` | App `DisableXdp` |
//! | POST | `/v1/xdp/stage` | `{"name","slot"}` | App `xdp stage` |
//! | POST | `/v1/xdp/activate` | `{"slot","generation","policy"}` | App `xdp activate`; policy `drop` or `pass` |
//! | POST | `/v1/xdp/rollback` | `{"policy"}` | App `xdp rollback` |
//! | GET | `/v1/xdp/status` | — | App `get xdp` boot id and slot roles |
//! | POST | `/v1/enable/exec` | `{}` | App `EnableExec` |
//! | POST | `/v1/disable/exec` | `{}` | App `DisableExec` |
//! | POST | `/v1/get/finish` | `{}` | host-only wait for `OK finish` |
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
//! `disable_xdp`, `enable_exec`, `disable_exec`, `get_finish`, `clear_map`,
//! `get_map`, `update_map`, `clear_perf`, `get_perf`, `get_serial`, `run`,
//! `get_status` / `dut_status`. **No probe.**
//!
//! ## Serial mapping (current `xpost-serial`)
//!
//! | HTTP | Host action |
//! |------|-------------|
//! | `flash/fw`, `flash/xdp`, `clear/xdp`, `get/xdp` | `SerialHandler` flash / cold-table API |
//! | `enable/xdp` / `disable/xdp` | `Command::EnableXdp` / `DisableXdp` |
//! | `enable/exec` / `disable/exec` | `Command::EnableExec` / `DisableExec` |
//! | `get/finish` | `Command::GetFinish` (host-only; eBPF `OK finish`) |
//! | `clear/map/{name}` | `Command::ClearMap { name }` |
//! | `get/map/{name}` | `Command::GetMap { name }` |
//! | `update/map/{name}` | `Command::UpdateMap { name, key, value }` |
//! | `clear/perf` / `get/perf` | `Command::ClearPerf` / `GetPerf` |
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
mod profiles;
mod state;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

/// Default rotating serial transcript directory (passed to xpost-serial).
const DEFAULT_LOG_DIR: &str = "target/xpost-agent";

#[derive(Parser, Debug)]
#[command(
    name = "xpost-agent",
    about = "Long-running HTTP/MCP daemon over xpost-serial (USB)"
)]
struct Cli {
    /// Bind address for `/health`, `/v1/*`, and `/mcp`.
    #[arg(long)]
    bind: String,

    /// Directory for rotating `serial.log` (xpost-serial transcript).
    #[arg(long, default_value = DEFAULT_LOG_DIR)]
    log_dir: PathBuf,
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
    http::serve_http(&cli.bind, &cli.log_dir)
}
