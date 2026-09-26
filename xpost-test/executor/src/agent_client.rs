//! Mac flash agent HTTP client (`/v1/*` on xpost-agent).

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

    /// Reserved for future executor flows (post-flash MCU reset).
    #[allow(dead_code)]
    pub async fn reboot(&self) -> Result<()> {
        // No dedicated /v1/reboot yet; keep API for callers.
        bail!("reboot not exposed on HTTP agent yet");
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
