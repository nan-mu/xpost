//! Long-running foreground HTTP service front-end for [`xpost_serial`].
//!
//! # HTTP control-plane contract
//!
//! The route implementations in `http.rs` define the current API.
//! This module documents their shared operations and JSON envelopes.
//! App wire verbs are [`xpost_serial::command::Command`] and
//! [`xpost_serial::reply::OkReply`].
//!
//! Run from `xpost-agent/`:
//! ```text
//! cargo run --release --locked -- serve --bind 0.0.0.0:9240
//! cargo run --release --locked -- serve --bind 0.0.0.0:9240 --log-dir target/xpost-agent
//! ```
//! The service exposes `/health` and `/v1/*` without authentication.
//! Long ops ~1200s. USB under `spawn_blocking` + `Mutex` on
//! [`xpost_serial::SerialHandler`]. Startup verifies the board with espflash Board Info.
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
//! + `ok:false`. Map decode failure → **502**.
//! Successful firmware flash and cold XDP installation also return `flash`.
//! `flash` has `firmware_profile`, `firmware_sha256`, and `containers`.
//! Each container has `program`, `mode`, `profile`, and `container_sha256`.
//! SHA-256 values use lowercase hex. A HAL flash returns an empty container list.
//! Hot slot copies keep their existing reply.
//! `GET /v1/preflight/medium` returns the release inventory and target readiness.
//! `GET /v1/preflight/xdp/{name}` returns the prepared container SHA-256.
//! Both preflight paths perform host checks only.
//!
//! ## Route table (frozen)
//!
//! | Method | Path | Body | Behavior |
//! |--------|------|------|----------|
//! | GET | `/health` | — | liveness |
//! | GET | `/v1/status` | — | `idle`, `busy`, or `stopping` |
//! | GET | `/v1/dut_status` | — | UART `ready` / `booting` (uses `ping`) |
//! | GET | `/v1/get/init` | — | host wait for unsolicited `OK init` |
//! | POST | `/v1/flash/fw` | `{"name","expected_sha256"?}` | Build and verify firmware and linked containers. Flash the retained bytes. |
//! | POST | `/v1/flash/xdp` | `{"name","slot"?,"firmware"?,"expected_sha256"?}` | Check identity and install. Provide both firmware fields to replace the only cold image. Provide a slot to preload. |
//! | POST | `/v1/clear/xdp` | `{"name": "*"\|"prog"\|["a","b"]}` | remove image(s); `*`=all; missing name → err |
//! | GET | `/v1/get/xdp` | — | list installed XDP images |
//! | POST | `/v1/enable/xdp` | `{}` | App `EnableXdp` |
//! | POST | `/v1/disable/xdp` | `{}` | App `DisableXdp` |
//! | POST | `/v1/xdp/switch` | `{"slot","generation","policy"}` | App `xdp switch`; policy `drop` or `pass` |
//! | GET | `/v1/xdp/status` | — | App `get xdp` boot id and slot roles |
//! | POST | `/v1/exec/batch` | `{"input_hex"}` | Upload, invoke, and read one 8258 B EXB2 buffer. |
//! | POST | `/v1/enable/exec` | `{}` | App `EnableExec` |
//! | POST | `/v1/disable/exec` | `{}` | App `DisableExec` |
//! | POST | `/v1/get/finish` | `{}` | host-only wait for `OK finish` |
//! | POST | `/v1/clear/map/{map_name}` | `{}` | App `ClearMap` |
//! | GET | `/v1/get/map/{map_name}` | — | App `GetMap` → **map JSON** |
//! | POST | `/v1/update/map/{map_name}` | see below | App `UpdateMap` (single or batch) |
//! | POST | `/v1/clear/perf` | `{}` | App `ClearPerf` |
//! | GET | `/v1/get/perf` | — | App `GetPerf` → b64 in `message` (or rich JSON) |
//! | POST | `/v1/perf/prepare` | `{"uuid","variant","profile"}` | App `perf prepare` |
//! | POST | `/v1/perf/arm` | `{"epoch"}` | App `perf arm` |
//! | POST | `/v1/perf/freeze` | `{"epoch"}` | App `perf freeze` |
//! | POST | `/v1/perf/read` | `{"epoch","offset","max_bytes"}` | App `perf read` |
//! | POST | `/v1/perf/release` | `{"epoch"}` | App `perf release` |
//! | POST | `/v1/get/serial` | `{"since_unix_ms","until_unix_ms"}` | transcript time window |
//! | POST | `/v1/run` | `{"cmd"}` | one App line, first `OK`/`ERR` only — **not** for get map/perf |
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
//! | `perf/prepare` | `Command::PerfPrepare` |
//! | `perf/arm` / `perf/freeze` / `perf/release` | `Command::PerfArm` / `PerfFreeze` / `PerfRelease` |
//! | `perf/read` | `Command::PerfRead` |
//! | `run` | parse line → `Command` (debug only) |
//!
//! Error details appear in `message`.
//!
//! ## Client notes (`xpost-test`)
//!
//! - Config `host:port` → client prefixes `http://`.
//! - Formal path: typed `/v1/flash/*`, `/v1/clear/*`, `/v1/get/*`,
//!   `/v1/enable|disable/xdp`, `/v1/update/map/*` only — never `/v1/run` for
//!   map/perf framing.
//! - Map seeding via agent serial `update/map`, not UDP MapCrud.

mod client;
mod config;
mod deployment;
mod device_lock;
#[cfg(feature = "test-device")]
mod fixture;
mod http;
mod ops;
mod preflight;
mod profiles;
mod release;
mod state;
mod util;

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};

use crate::config::{Location, ServeArgs};

#[derive(Parser)]
#[command(
    version,
    about = "Serve USB device operations or send typed HTTP commands"
)]
struct Cli {
    /// Select an explicit HTTP origin for a remote operation.
    #[arg(long, global = true)]
    endpoint: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP service in the foreground.
    Serve(ServeArgs),
    /// Check release deployments without USB. Use --remote to query HTTP.
    Preflight {
        /// Prepare one named profile against its existing Firmware ELF.
        #[arg(long, conflicts_with = "inventory")]
        xdp: Option<String>,
        /// Check the release archive inventory without target firmware.
        #[arg(long, conflicts_with = "remote")]
        inventory: bool,
        /// Query the service's host-only preflight route.
        #[arg(long)]
        remote: bool,
    },
    #[command(flatten)]
    Device(client::DeviceCommand),
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xpost-agent: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Device(command) => client::run(command, cli.endpoint.as_deref()),
        Command::Serve(args) => {
            ensure!(cli.endpoint.is_none(), "serve uses --bind");
            http::serve_http(&args.bind, &args.log_dir, Location::resolve()?)
        }
        Command::Preflight {
            xdp, remote: true, ..
        } => client::preflight(xdp, cli.endpoint.as_deref()),
        Command::Preflight { xdp, inventory, .. } => {
            ensure!(
                cli.endpoint.is_none(),
                "use preflight --remote with --endpoint"
            );
            let root = Location::resolve()?.root()?;
            if inventory {
                let report = preflight::inventory(&root);
                print_json(&report)?;
                ensure!(report.deployment_ready(), "release inventory failed");
                return Ok(());
            }
            let profiles = state::load_profiles(&root)?;
            if let Some(name) = xdp {
                return print_json(&preflight::profile(&root, &profiles, &name)?);
            }
            let report = preflight::medium(&root, &profiles);
            print_json(&report)?;
            ensure!(
                report.deployment_ready(),
                "medium deployment preflight failed: {}/{} verified release deployments, {} target layouts, {} issues",
                report.deployments.len(),
                report.required,
                report.prepared.len(),
                report.issues.len()
            );
            Ok(())
        }
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}
