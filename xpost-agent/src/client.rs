use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Subcommand, ValueEnum};
use reqwest::blocking::ClientBuilder;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use crate::config::{Build, Location};
use crate::lifecycle::{INSTANCE_HEADER, Phase, RuntimeDir, print_json};

const TIMEOUT: Duration = Duration::from_secs(1210);

#[derive(Subcommand)]
pub enum DeviceCommand {
    /// Check HTTP liveness.
    Health,
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
    Fw { name: String },
    /// Install a cold XDP ELF or copy it into a SRAM slot.
    Xdp {
        name: String,
        /// Copy an installed image into this slot without resetting the device.
        #[arg(long)]
        slot: Option<u8>,
    },
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
    /// Read the existing Agent HTTP status.
    Status,
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
            Self::Dut {
                command: Dut::Status,
            } => Request::get("/v1/dut_status"),
            Self::Flash {
                command: Flash::Fw { name },
            } => Request::post("/v1/flash/fw", json!({ "name": name })),
            Self::Flash {
                command: Flash::Xdp { name, slot },
            } => {
                let body = match slot {
                    Some(slot) => json!({ "name": name, "slot": slot }),
                    None => json!({ "name": name }),
                };
                Request::post("/v1/flash/xdp", body)
            }
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
            Self::Get {
                command: Get::Status,
            } => Request::get("/v1/status"),
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
    let request = command.request()?;
    let (base, instance) = match endpoint {
        Some(endpoint) => (endpoint_url(endpoint)?, None),
        None => {
            let location = Location::resolve()?;
            let runtime = RuntimeDir::open(&location.worktree)?;
            let status = runtime
                .query()?
                .context("no daemon for this Worktree. Run xpost-agent start")?;
            ensure!(
                status.state == Phase::Running,
                "daemon is {:?}",
                status.state
            );
            let instance = status.instance;
            let build = Build::current()?;
            ensure!(
                instance.build.image_id == build.image_id,
                "daemon is running {} (image {}, Git {}). Build {} was requested. Run xpost-agent restart",
                instance.build.executable.display(),
                instance.build.image_id,
                instance.build.git_revision.as_deref().unwrap_or("unknown"),
                build.image_id,
            );
            (endpoint_url(&instance.endpoint)?, Some(instance.id))
        }
    };
    let url = request.url(base)?;
    let method = if request.body.is_some() {
        Method::POST
    } else {
        Method::GET
    };
    let client = builder(TIMEOUT).build()?;
    let mut call = client.request(method, url.clone());
    if let Some(id) = instance {
        call = call.header(INSTANCE_HEADER, id.to_string());
    }
    if let Some(body) = request.body {
        call = call.json(&body);
    }
    let response = call
        .send()
        .with_context(|| format!("send request to {url}"))?;
    let status = response.status();
    let bytes = response.bytes()?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) => {
            print_json(&value)?;
            ensure!(status.is_success(), "HTTP {status} from {url}");
            match value.get("ok").and_then(Value::as_bool) {
                Some(true) => Ok(()),
                Some(false) => bail!("daemon reported failure: {}", value["message"]),
                None => bail!("response has no boolean ok field"),
            }
        }
        Err(error) if status.is_success() => Err(error).context("decode HTTP response JSON"),
        Err(_) => bail!(
            "HTTP {status} from {url}: {}",
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
