//! Mac flash / DUT agent HTTP client — frozen `/v1/*` contract on xpost-agent.
//!
//! Source of truth: `xpost-agent/src/main.rs` rustdoc
//! ("HTTP / MCP control-plane contract (frozen)").

use std::net::Ipv4Addr;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::time::Duration;

#[derive(Clone)]
pub struct AgentClient {
    base: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct ApiResp {
    ok: bool,
    message: String,
}

/// `GET /v1/get/map/{name}` frozen JSON body (fields we read).
#[derive(Debug, Clone, Deserialize)]
pub struct MapGetResp {
    pub ok: bool,
    /// Present on the frozen map JSON shape (absent on `{ok,message}` envelopes).
    #[allow(dead_code)]
    pub name: String,
    #[serde(default)]
    pub entries: Vec<MapEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MapEntry {
    pub key: String,
    pub value: String,
}

impl AgentClient {
    pub fn new(endpoint: String) -> Self {
        let base = normalize_base(&endpoint);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(1200))
            .build()
            .expect("reqwest client");
        Self { base, http }
    }

    /// `POST /v1/flash/fw` — build+flash firmware profile (does not install xdp.elf).
    pub async fn flash_fw(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        let msg = self.post_json("/v1/flash/fw", &body).await?;
        log::info!("flash_fw: {msg}");
        Ok(())
    }

    /// `POST /v1/enable/xdp`.
    pub async fn enable_xdp(&self) -> Result<String> {
        self.post_json("/v1/enable/xdp", &serde_json::json!({})).await
    }

    /// `POST /v1/disable/xdp`.
    pub async fn disable_xdp(&self) -> Result<String> {
        self.post_json("/v1/disable/xdp", &serde_json::json!({})).await
    }

    /// `POST /v1/clear/map/{name}`.
    pub async fn clear_map(&self, name: &str) -> Result<String> {
        let path = format!("/v1/clear/map/{name}");
        self.post_json(&path, &serde_json::json!({})).await
    }

    /// `GET /v1/get/map/{name}` → map JSON.
    pub async fn get_map(&self, name: &str) -> Result<MapGetResp> {
        let path = format!("/v1/get/map/{name}");
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .with_context(|| format!("read GET {url}"))?;
        if !status.is_success() {
            bail!("GET {url}: HTTP {status} — {body}");
        }
        // Prefer rich map JSON; fall back to envelope error.
        if let Ok(map) = serde_json::from_str::<MapGetResp>(&body) {
            if !map.ok {
                bail!("get map {name}: ok=false");
            }
            return Ok(map);
        }
        let api: ApiResp =
            serde_json::from_str(&body).with_context(|| format!("decode GET {url}: {body}"))?;
        if !api.ok {
            bail!("{}", api.message);
        }
        bail!("GET {url}: unexpected body (want map JSON): {body}");
    }

    /// Read one ARRAY slot from `GET /v1/get/map/{name}`.
    pub async fn map_slot(&self, name: &str, slot: u32) -> Result<u64> {
        let map = self.get_map(name).await?;
        let key = slot.to_string();
        for e in &map.entries {
            if e.key == key {
                return e
                    .value
                    .parse()
                    .with_context(|| format!("map {name} slot {slot} value"));
            }
        }
        bail!("map {name} missing slot {slot}");
    }

    /// `POST /v1/update/map/{name}` — batch `entries`.
    pub async fn update_map_batch(&self, name: &str, entries: &[(String, String)]) -> Result<String> {
        let path = format!("/v1/update/map/{name}");
        let entries: Vec<serde_json::Value> = entries
            .iter()
            .map(|(k, v)| serde_json::json!({ "key": k, "value": v }))
            .collect();
        let body = serde_json::json!({ "entries": entries });
        self.post_json(&path, &body).await
    }

    /// Convenience: seed HASH map with IPv4 keys (value `"0"`).
    pub async fn update_map_ips(&self, name: &str, ips: &[Ipv4Addr]) -> Result<String> {
        let entries: Vec<(String, String)> = ips
            .iter()
            .map(|ip| (ip.to_string(), "0".to_string()))
            .collect();
        self.update_map_batch(name, &entries).await
    }

    /// `POST /v1/clear/perf`.
    pub async fn clear_perf(&self) -> Result<String> {
        self.post_json("/v1/clear/perf", &serde_json::json!({})).await
    }

    /// `GET /v1/get/perf` — message carries `b64=…` (or equivalent).
    pub async fn get_perf(&self) -> Result<String> {
        self.get_message("/v1/get/perf").await
    }

    pub async fn wait_idle(&self, budget: Duration) -> Result<()> {
        let start = tokio::time::Instant::now();
        while start.elapsed() < budget {
            let msg = self.get_message("/v1/status").await?;
            if msg.trim() == "idle" {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("agent not idle within {:?}", budget);
    }

    /// `GET /v1/dut_status` — UART `ready …` / `booting` (agent may ping internally).
    #[allow(dead_code)]
    pub async fn dut_status(&self) -> Result<String> {
        self.get_message("/v1/dut_status").await
    }

    /// `GET /v1/get/init` — wait for unsolicited `OK init`.
    pub async fn get_init(&self) -> Result<String> {
        self.get_message("/v1/get/init").await
    }

    async fn get_message(&self, path: &str) -> Result<String> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        let api: ApiResp = resp
            .json()
            .await
            .with_context(|| format!("decode GET {url}"))?;
        if !status.is_success() {
            bail!("GET {url}: HTTP {status} — {}", api.message);
        }
        if !api.ok {
            bail!("{}", api.message);
        }
        Ok(api.message)
    }

    async fn post_json(&self, path: &str, body: &serde_json::Value) -> Result<String> {
        let url = format!("{}{path}", self.base);
        let resp = self
            .http
            .post(&url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let api: ApiResp = resp
            .json()
            .await
            .with_context(|| format!("decode POST {url}"))?;
        if !status.is_success() {
            bail!("POST {url}: HTTP {status} — {}", api.message);
        }
        if !api.ok {
            bail!("{}", api.message);
        }
        Ok(api.message)
    }
}

fn normalize_base(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if e.starts_with("http://") || e.starts_with("https://") {
        e.to_string()
    } else {
        format!("http://{e}")
    }
}
