use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use reqwest::blocking::ClientBuilder;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use crate::config::Location;
use crate::http::WORKTREE_HEADER;
use crate::print_json;

const TIMEOUT: Duration = Duration::from_secs(1210);

#[derive(Subcommand)]
pub enum DeviceCommand {
    /// Check HTTP liveness.
    Health,
    /// Read the Agent's current device-operation status.
    Status,
    /// Submit one typed EXB2 invocation.
    Exec {
        #[command(subcommand)]
        command: Exec,
    },
    /// Control the Perf session and read export chunks.
    Perf {
        #[command(subcommand)]
        command: Perf,
    },
    /// Query the device through the daemon.
    Dut {
        #[command(subcommand)]
        command: Dut,
    },
    /// Install firmware or an XDP ELF.
    Flash {
        #[command(subcommand)]
        command: Flash,
    },
    /// Clear images, map contents, or the performance report.
    Clear {
        #[command(subcommand)]
        command: Clear,
    },
    /// Read results or wait for an App marker.
    Get {
        #[command(subcommand)]
        command: Get,
    },
    /// Update a map through the device protocol.
    Update {
        #[command(subcommand)]
        command: Update,
    },
    /// Enable XDP ingress or invoke the exec workload.
    Enable { target: Feature },
    /// Disable XDP ingress or exec.
    Disable { target: Feature },
    /// Publish a staged XDP image or read slot state.
    Xdp {
        #[command(subcommand)]
        command: Xdp,
    },
    /// Send one debug App command. Use typed get commands for maps and perf.
    Run { cmd: String },
}

#[derive(Subcommand)]
pub enum Dut {
    /// Check whether the App replies to ping.
    Status,
}

#[derive(Subcommand)]
pub enum Flash {
    /// Build and flash a Firmware ELF by profile name.
    Fw {
        name: String,
        #[arg(long)]
        expected_sha256: Option<String>,
    },
    /// Install a cold XDP ELF or copy it into a SRAM slot.
    Xdp {
        name: String,
        /// Copy an installed image into this slot without resetting the device.
        #[arg(long)]
        slot: Option<u8>,
        /// Bind a cold install to this Firmware ELF profile.
        #[arg(long, requires = "expected_sha256", conflicts_with = "slot")]
        firmware: Option<String>,
        /// Expected SHA-256 of the Firmware ELF for a cold install.
        #[arg(long, requires = "firmware", conflicts_with = "slot")]
        expected_sha256: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum Exec {
    /// Upload, invoke once, and read a complete EXB2 buffer.
    Batch {
        /// Complete 8258-byte input as hexadecimal text.
        #[arg(
            long,
            required_unless_present = "input_file",
            conflicts_with = "input_file"
        )]
        input_hex: Option<String>,
        /// File containing the complete binary EXB2 buffer.
        #[arg(long)]
        input_file: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum Perf {
    /// Prepare a Metrics or Trace capture for the run and firmware.
    Prepare {
        #[arg(long)]
        uuid: String,
        #[arg(long)]
        variant: Variant,
        #[arg(long)]
        profile: String,
    },
    /// Begin capture for the prepared epoch.
    Arm {
        #[arg(long)]
        epoch: u32,
    },
    /// Freeze capture for the epoch.
    Freeze {
        #[arg(long)]
        epoch: u32,
    },
    /// Read one chunk of the frozen export.
    Read {
        #[arg(long)]
        epoch: u32,
        #[arg(long)]
        offset: u32,
        #[arg(long)]
        max_bytes: u32,
    },
    /// Release the epoch after successful export.
    Release {
        #[arg(long)]
        epoch: u32,
    },
}

#[derive(ValueEnum, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    Metrics,
    Trace,
}

#[derive(Subcommand)]
pub enum Clear {
    /// Remove one or more cold XDP ELFs. Quote '*' to remove all.
    Xdp {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Clear a named map.
    Map { name: String },
    /// Clear performance data.
    Perf,
}

#[derive(Subcommand)]
pub enum Get {
    /// Wait for the unsolicited App initialization marker.
    Init,
    /// Wait for the unsolicited App completion marker.
    Finish,
    /// List installed cold XDP ELFs.
    Xdp,
    /// Read a map as structured JSON.
    Map { name: String },
    /// Read the framed performance data.
    Perf,
    /// Read a serial transcript window in Unix milliseconds.
    Serial {
        #[arg(long)]
        since_unix_ms: u64,
        #[arg(long)]
        until_unix_ms: u64,
    },
}

#[derive(Subcommand)]
pub enum Update {
    /// Write one entry or a JSON array of entries in order.
    Map(UpdateMap),
}

#[derive(Args)]
pub struct UpdateMap {
    name: String,
    /// Single entry key (ASCII token).
    #[arg(long, requires = "value", required_unless_present = "entries")]
    key: Option<String>,
    /// Single entry value (ASCII token).
    #[arg(long, requires = "key")]
    value: Option<String>,
    /// Batch array of objects with string "key" and "value" fields.
    #[arg(long, conflicts_with_all = ["key", "value"])]
    entries: Option<String>,
}

#[derive(ValueEnum, Clone, Copy)]
pub enum Feature {
    Xdp,
    Exec,
}

impl Feature {
    fn path(self) -> &'static str {
        match self {
            Self::Xdp => "xdp",
            Self::Exec => "exec",
        }
    }
}

#[derive(Subcommand)]
pub enum Xdp {
    /// Publish a staged image with the expected slot generation.
    Switch {
        #[arg(long)]
        slot: u8,
        #[arg(long)]
        generation: u32,
        #[arg(long)]
        policy: Policy,
    },
    /// Read the App's XDP slot state.
    Status,
}

#[derive(ValueEnum, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Drop,
    Pass,
}

struct Request {
    path: String,
    map: Option<String>,
    body: Option<Value>,
}

impl Request {
    fn get(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            map: None,
            body: None,
        }
    }

    fn post(path: impl Into<String>, body: Value) -> Self {
        Self {
            path: path.into(),
            map: None,
            body: Some(body),
        }
    }

    fn map(mut self, name: String) -> Self {
        self.map = Some(name);
        self
    }

    fn url(&self, mut base: Url) -> Result<Url> {
        base.set_path(&self.path);
        if let Some(name) = &self.map {
            ensure!(
                !name.is_empty() && !matches!(name.as_str(), "." | ".."),
                "invalid map name"
            );
            base.path_segments_mut()
                .map_err(|_| anyhow::anyhow!("invalid HTTP endpoint"))?
                .push(name);
        }
        Ok(base)
    }
}

impl DeviceCommand {
    fn request(self) -> Result<Request> {
        let empty = || json!({});
        Ok(match self {
            Self::Health => Request::get("/health"),
            Self::Status => Request::get("/v1/status"),
            Self::Dut {
                command: Dut::Status,
            } => Request::get("/v1/dut_status"),
            Self::Flash {
                command:
                    Flash::Fw {
                        name,
                        expected_sha256,
                    },
            } => {
                let mut body = json!({ "name": name });
                if let Some(digest) = expected_sha256 {
                    body["expected_sha256"] = json!(digest);
                }
                Request::post("/v1/flash/fw", body)
            }
            Self::Flash {
                command:
                    Flash::Xdp {
                        name,
                        slot,
                        firmware,
                        expected_sha256,
                    },
            } => {
                let mut body = json!({ "name": name });
                if let Some(slot) = slot {
                    body["slot"] = json!(slot);
                }
                if let (Some(firmware), Some(digest)) = (firmware, expected_sha256) {
                    body["firmware"] = json!(firmware);
                    body["expected_sha256"] = json!(digest);
                }
                Request::post("/v1/flash/xdp", body)
            }
            Self::Exec {
                command:
                    Exec::Batch {
                        input_hex,
                        input_file,
                    },
            } => {
                let input_hex = match input_file {
                    Some(path) => std::fs::read(&path)
                        .with_context(|| format!("read EXB2 file {}", path.display()))?
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    None => input_hex.context("missing EXB2 input")?,
                };
                Request::post("/v1/exec/batch", json!({ "input_hex": input_hex }))
            }
            Self::Perf { command } => match command {
                Perf::Prepare {
                    uuid,
                    variant,
                    profile,
                } => Request::post(
                    "/v1/perf/prepare",
                    json!({ "uuid": uuid, "variant": variant, "profile": profile }),
                ),
                Perf::Arm { epoch } => Request::post("/v1/perf/arm", json!({ "epoch": epoch })),
                Perf::Freeze { epoch } => {
                    Request::post("/v1/perf/freeze", json!({ "epoch": epoch }))
                }
                Perf::Read {
                    epoch,
                    offset,
                    max_bytes,
                } => Request::post(
                    "/v1/perf/read",
                    json!({ "epoch": epoch, "offset": offset, "max_bytes": max_bytes }),
                ),
                Perf::Release { epoch } => {
                    Request::post("/v1/perf/release", json!({ "epoch": epoch }))
                }
            },
            Self::Clear {
                command: Clear::Xdp { names },
            } => {
                let name = if names.len() == 1 {
                    json!(names[0])
                } else {
                    json!(names)
                };
                Request::post("/v1/clear/xdp", json!({ "name": name }))
            }
            Self::Clear {
                command: Clear::Map { name },
            } => Request::post("/v1/clear/map", empty()).map(name),
            Self::Clear {
                command: Clear::Perf,
            } => Request::post("/v1/clear/perf", empty()),
            Self::Get { command: Get::Init } => Request::get("/v1/get/init"),
            Self::Get {
                command: Get::Finish,
            } => Request::post("/v1/get/finish", empty()),
            Self::Get { command: Get::Xdp } => Request::get("/v1/get/xdp"),
            Self::Get {
                command: Get::Map { name },
            } => Request::get("/v1/get/map").map(name),
            Self::Get { command: Get::Perf } => Request::get("/v1/get/perf"),
            Self::Get {
                command:
                    Get::Serial {
                        since_unix_ms,
                        until_unix_ms,
                    },
            } => Request::post(
                "/v1/get/serial",
                json!({ "since_unix_ms": since_unix_ms, "until_unix_ms": until_unix_ms }),
            ),
            Self::Update {
                command: Update::Map(args),
            } => {
                let body = match args.entries {
                    Some(entries) => {
                        let entries: Vec<MapEntry> = serde_json::from_str(&entries)
                            .context("--entries must be a JSON array of key/value objects")?;
                        ensure!(
                            !entries.is_empty(),
                            "--entries must contain at least one entry"
                        );
                        json!({ "entries": entries })
                    }
                    None => json!({ "key": args.key, "value": args.value }),
                };
                Request::post("/v1/update/map", body).map(args.name)
            }
            Self::Enable { target } => {
                Request::post(format!("/v1/enable/{}", target.path()), empty())
            }
            Self::Disable { target } => {
                Request::post(format!("/v1/disable/{}", target.path()), empty())
            }
            Self::Xdp {
                command:
                    Xdp::Switch {
                        slot,
                        generation,
                        policy,
                    },
            } => Request::post(
                "/v1/xdp/switch",
                json!({ "slot": slot, "generation": generation, "policy": policy }),
            ),
            Self::Xdp {
                command: Xdp::Status,
            } => Request::get("/v1/xdp/status"),
            Self::Run { cmd } => Request::post("/v1/run", json!({ "cmd": cmd })),
        })
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct MapEntry {
    key: String,
    value: String,
}

pub fn builder(timeout: Duration) -> ClientBuilder {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(2))
        .timeout(timeout)
}

pub fn run(command: DeviceCommand, endpoint: Option<&str>) -> Result<()> {
    send(command.request()?, endpoint)
}

pub fn preflight(name: Option<String>, endpoint: Option<&str>) -> Result<()> {
    let request = match name {
        Some(name) => Request::get("/v1/preflight/xdp").map(name),
        None => Request::get("/v1/preflight/medium"),
    };
    send(request, endpoint)
}

fn send(request: Request, endpoint: Option<&str>) -> Result<()> {
    let base = endpoint_url(endpoint.unwrap_or("127.0.0.1:9240"))?;
    let client = builder(TIMEOUT).build()?;
    let worktree = if endpoint.is_none() {
        let worktree = Location::resolve()?.worktree_id();
        let check = builder(Duration::from_secs(2)).build()?
            .get(base.join("/health")?)
            .header(WORKTREE_HEADER, &worktree)
            .send()
            .context("connect to local Agent. Run cargo run --release --locked -- serve from xpost-agent")?;
        ensure!(
            check.status().is_success()
                && check
                    .headers()
                    .get(WORKTREE_HEADER)
                    .is_some_and(|id| id == &worktree),
            "local Agent did not identify this Worktree"
        );
        let health: Value = check.json().context("decode local Agent health response")?;
        ensure!(health["ok"] == true, "local Agent health check failed");
        Some(worktree)
    } else {
        None
    };
    let url = request.url(base)?;
    let operation = request.body.is_some();
    let method = if operation { Method::POST } else { Method::GET };
    let mut call = client.request(method, url.clone());
    if let Some(worktree) = worktree {
        call = call.header(WORKTREE_HEADER, worktree);
    }
    if let Some(body) = request.body {
        call = call.json(&body);
    }
    let response = call
        .send()
        .map_err(|error| {
            if operation && error.is_timeout() {
                anyhow::Error::from(error).context("operation outcome unknown")
            } else {
                error.into()
            }
        })
        .with_context(|| format!("send request to {url}"))?;
    let status = response.status();
    let bytes = response.bytes()?;
    let status_context = if operation && status == reqwest::StatusCode::REQUEST_TIMEOUT {
        " (operation outcome unknown)"
    } else {
        ""
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => {
            print_json(&value)?;
            ensure!(
                status.is_success(),
                "HTTP {status} from {url}{status_context}"
            );
            match value.get("ok").and_then(Value::as_bool) {
                Some(true) => Ok(()),
                Some(false) => bail!(
                    "Agent reported failure: {}",
                    value["message"].as_str().unwrap_or("see JSON response")
                ),
                None => bail!("response has no boolean ok field"),
            }
        }
        Err(error) if status.is_success() => Err(error).context("decode HTTP response JSON"),
        Err(_) => bail!(
            "HTTP {status} from {url}{status_context}: {}",
            String::from_utf8_lossy(&bytes)
        ),
    }
}

fn endpoint_url(endpoint: &str) -> Result<Url> {
    let normalized = if endpoint.contains("://") {
        endpoint.to_owned()
    } else {
        format!("http://{endpoint}")
    };
    let url = Url::parse(&normalized).context("parse HTTP endpoint")?;
    ensure!(
        url.scheme() == "http"
            && url.host().is_some()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "expected an HTTP origin such as http://127.0.0.1:9240"
    );
    Ok(url)
}
