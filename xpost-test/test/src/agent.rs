//! Mac flash / DUT agent HTTP client (`/v1/*` on xpost-agent).

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

impl AgentClient {
    pub fn new(endpoint: String) -> Self {
        let base = normalize_base(&endpoint);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(1200))
            .build()
            .expect("reqwest client");
        Self { base, http }
    }

    pub async fn flash_profile(&self, profile_id: &str) -> Result<()> {
        let body = serde_json::json!({ "profile_id": profile_id });
        let msg = self.post_json("/v1/flash_profile", &body).await?;
        log::info!("flash_profile: {msg}");
        Ok(())
    }

    pub async fn wait_idle(&self, budget: Duration) -> Result<()> {
        let start = tokio::time::Instant::now();
        while start.elapsed() < budget {
            let msg = self.get_json("/v1/status").await?;
            if msg.trim() == "idle" {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("agent not idle within {:?}", budget);
    }

    /// UART scrape: `ready …` or `booting` (see xpost-agent `/v1/dut_status`).
    pub async fn dut_status(&self) -> Result<String> {
        self.get_json("/v1/dut_status").await
    }

    /// Install XDP ELF via the agent's USB session.
    pub async fn program(&self, elf: &str) -> Result<String> {
        let body = serde_json::json!({ "elf": elf });
        self.post_json("/v1/program", &body).await
    }

    pub async fn clear_xdp(&self) -> Result<()> {
        let _ = self.post_json("/v1/clear", &serde_json::json!({})).await?;
        Ok(())
    }

    pub async fn counters(&self) -> Result<String> {
        self.post_json("/v1/counters", &serde_json::json!({})).await
    }

    /// UDP stim through the agent. Returns `ack` or `no-ack`.
    pub async fn probe(&self, esp: &str, port: u16) -> Result<String> {
        let body = serde_json::json!({ "esp": esp, "port": port });
        self.post_json("/v1/probe", &body).await
    }

    /// Fire-and-forget N STIMs (no ACK wait). Returns how many were sent.
    pub async fn probe_burst(&self, esp: &str, port: u16, n: u32) -> Result<u32> {
        let body = serde_json::json!({ "esp": esp, "port": port, "count": n });
        let msg = self.post_json("/v1/probe_burst", &body).await?;
        let sent = msg
            .trim()
            .strip_prefix("sent=")
            .context("probe_burst reply missing sent=")?
            .parse()
            .context("probe_burst sent=")?;
        Ok(sent)
    }

    /// One serial App-mode request (e.g. `rxcnt 17`).
    pub async fn request(&self, cmd: &str) -> Result<String> {
        let body = serde_json::json!({ "cmd": cmd });
        self.post_json("/v1/request", &body).await
    }

    /// Host-timestamped UART/cmd transcript for `[since_unix_ms, until_unix_ms]`.
    ///
    /// Manual capture of `tsflow` / `prof` lines. Formal and `dev` runs do not
    /// call this: printing during the measurement window contends with Wi-Fi TX.
    #[allow(dead_code, reason = "manual serial_window client; automated runs stay USB-quiet")]
    pub async fn serial_window(&self, since_unix_ms: u64, until_unix_ms: u64) -> Result<String> {
        let body = serde_json::json!({
            "since_unix_ms": since_unix_ms,
            "until_unix_ms": until_unix_ms,
        });
        self.post_json("/v1/serial_window", &body).await
    }

    async fn get_json(&self, path: &str) -> Result<String> {
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
