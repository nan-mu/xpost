//! Mac flash / DUT agent HTTP client — frozen `/v1/*` contract on xpost-agent.
//!
//! The daemon route table is in `xpost-agent/src/http.rs`.

use std::net::Ipv4Addr;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
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
    flash: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct MediumPreflight {
    ok: bool,
    preflight: Option<DeploymentReport>,
    message: Option<String>,
}

#[derive(Deserialize)]
pub struct DeploymentReport {
    pub release: String,
    required: usize,
    deployments: Vec<serde_json::Value>,
    issues: Vec<String>,
    #[serde(default)]
    pub prepared: Vec<PreparedDeployment>,
    #[serde(default)]
    pub hal_firmware: Vec<FirmwareIdentity>,
}

#[derive(Deserialize)]
pub struct PreparedDeployment {
    pub program: String,
    pub mode: String,
    pub profile: String,
    pub slot: u8,
    #[serde(flatten)]
    pub firmware: FirmwareIdentity,
}

#[derive(Deserialize)]
pub struct FirmwareIdentity {
    pub firmware_profile: String,
    pub firmware_sha256: String,
}

#[derive(Deserialize)]
struct ExecBatchResponse {
    ok: bool,
    result: Option<ExecBatchWire>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct ExecBatchWire {
    boot_id: u32,
    begin_tick: u64,
    end_tick: u64,
    execution_return: u32,
    output: String,
}

#[derive(Clone, Copy, serde::Serialize)]
pub struct PerfFreeze {
    pub tick: u64,
    pub flags: u32,
    total: usize,
    digest: u32,
}

pub struct ExecBatch {
    pub boot_id: u32,
    pub begin_tick: u64,
    pub end_tick: u64,
    pub execution_return: u32,
    pub output: Vec<u8>,
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

    pub async fn health(&self) -> Result<()> {
        self.get_message("/health").await?;
        Ok(())
    }

    pub async fn medium_preflight(&self) -> Result<DeploymentReport> {
        let response: MediumPreflight = self
            .http
            .get(format!("{}/v1/preflight/medium", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let report = response.preflight.with_context(|| {
            response
                .message
                .unwrap_or_else(|| "Agent returned no deployment preflight report".into())
        })?;
        anyhow::ensure!(
            response.ok,
            "libbpf-mcu {}: {}/{} deployments verified; {}",
            report.release,
            report.deployments.len(),
            report.required,
            report.issues.join("\n")
        );
        Ok(report)
    }

    pub async fn preflight_xdp(&self, name: &str) -> Result<String> {
        let result: ApiResp = self
            .http
            .get(format!("{}/v1/preflight/xdp/{name}", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(result.ok, "{}", result.message);
        Ok(result.message)
    }

    pub async fn exb2(&self, input_hex: &str) -> Result<ExecBatch> {
        let response: ExecBatchResponse = self
            .http
            .post(format!("{}/v1/exec/batch", self.base))
            .json(&serde_json::json!({ "input_hex": input_hex }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        anyhow::ensure!(
            response.ok,
            "{}",
            response.message.as_deref().unwrap_or("EXB2 request failed")
        );
        let wire = response
            .result
            .context("Agent EXB2 response lacks a result")?;
        Ok(ExecBatch {
            boot_id: wire.boot_id,
            begin_tick: wire.begin_tick,
            end_tick: wire.end_tick,
            execution_return: wire.execution_return,
            output: STANDARD.decode(wire.output)?,
        })
    }

    pub async fn perf_prepare(
        &self,
        run: uuid::Uuid,
        profile: &str,
        variant: &str,
    ) -> Result<(u32, u64)> {
        let reply = self
            .post_json(
                "/v1/perf/prepare",
                &serde_json::json!({
                    "uuid": run.simple().to_string(), "profile": profile, "variant": variant
                }),
            )
            .await?;
        Ok((
            perf_field(&reply, "epoch")?.parse()?,
            perf_field(&reply, "boot_id")?.parse()?,
        ))
    }

    pub async fn perf_arm(&self, epoch: u32) -> Result<u64> {
        let reply = self
            .post_json("/v1/perf/arm", &serde_json::json!({ "epoch": epoch }))
            .await?;
        Ok(perf_field(&reply, "tick")?.parse()?)
    }

    pub async fn perf_freeze(&self, epoch: u32) -> Result<PerfFreeze> {
        let frozen = self
            .post_json("/v1/perf/freeze", &serde_json::json!({ "epoch": epoch }))
            .await?;
        Ok(PerfFreeze {
            tick: perf_field(&frozen, "tick")?.parse()?,
            flags: perf_field(&frozen, "flags")?.parse()?,
            total: perf_field(&frozen, "len")?.parse()?,
            digest: perf_field(&frozen, "digest")?.parse()?,
        })
    }

    pub async fn perf_read(&self, epoch: u32, frozen: PerfFreeze) -> Result<Vec<u8>> {
        let PerfFreeze { total, digest, .. } = frozen;
        anyhow::ensure!(
            (57..=64 * 1024).contains(&total),
            "invalid perf export length {total}"
        );
        let mut bytes = Vec::with_capacity(total);
        while bytes.len() < total {
            let part = self
                .post_json(
                    "/v1/perf/read",
                    &serde_json::json!({
                        "epoch": epoch, "offset": bytes.len(), "max_bytes": 512
                    }),
                )
                .await?;
            anyhow::ensure!(
                perf_field(&part, "total")?.parse::<usize>()? == total
                    && perf_field(&part, "digest")?.parse::<u32>()? == digest,
                "perf identity changed during export"
            );
            let chunk = STANDARD.decode(perf_field(&part, "b64")?)?;
            anyhow::ensure!(
                !chunk.is_empty()
                    && chunk.len() == perf_field(&part, "n")?.parse::<usize>()?
                    && bytes.len() + chunk.len() <= total,
                "invalid perf export chunk"
            );
            bytes.extend_from_slice(&chunk);
        }
        let observed_digest = bytes.iter().fold(0x811c9dc5_u32, |hash, byte| {
            (hash ^ u32::from(*byte)).wrapping_mul(0x01000193)
        });
        anyhow::ensure!(observed_digest == digest, "perf export digest mismatch");
        Ok(bytes)
    }

    pub async fn perf_release(&self, epoch: u32) -> Result<()> {
        self.post_json("/v1/perf/release", &serde_json::json!({ "epoch": epoch }))
            .await?;
        Ok(())
    }

    /// Build firmware and preflight all linked XDP artifacts before any device write.
    /// Then flash firmware and install the verified cached containers.
    /// The Agent selects XDP ELFs from its pinned libbpf-mcu release.
    pub async fn flash_fw(&self, name: &str) -> Result<()> {
        let body = serde_json::json!({ "name": name });
        let msg = self.post_json("/v1/flash/fw", &body).await?;
        log::info!("flash_fw: {msg}");
        Ok(())
    }

    pub async fn flash_fw_evidence(
        &self,
        name: &str,
        expected_sha256: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.post_response(
            "/v1/flash/fw",
            &serde_json::json!({ "name": name, "expected_sha256": expected_sha256 }),
        )
        .await?
        .flash
        .context("Agent did not return the flashed firmware identity")
    }

    /// Replace a single installed image using its checked Firmware ELF and partition.
    pub async fn bind_xdp(&self, name: &str, firmware: &str, sha256: &str) -> Result<()> {
        let body =
            serde_json::json!({ "name": name, "firmware": firmware, "expected_sha256": sha256 });
        let message = self.post_json("/v1/flash/xdp", &body).await?;
        log::info!("bind_xdp: {message}");
        Ok(())
    }

    /// `POST /v1/flash/xdp` with `slot`. Copies an image already in the cold
    /// table into that slot. Does not reset. Reply is `OK xdp flash <slot> <gen>`.
    pub async fn flash_xdp_slot(&self, name: &str, slot: u8) -> Result<String> {
        self.post_json(
            "/v1/flash/xdp",
            &serde_json::json!({ "name": name, "slot": slot }),
        )
        .await
    }

    /// `POST /v1/clear/xdp` — `name` `"*"` clears the whole cold table.
    pub async fn enable_xdp(&self) -> Result<String> {
        self.post_json("/v1/enable/xdp", &serde_json::json!({}))
            .await
    }

    /// `POST /v1/disable/xdp`.
    pub async fn disable_xdp(&self) -> Result<String> {
        self.post_json("/v1/disable/xdp", &serde_json::json!({}))
            .await
    }

    /// `POST /v1/xdp/switch`. `policy` is `drop` or `pass`.
    pub async fn switch_xdp(&self, slot: u8, generation: u32, policy: &str) -> Result<String> {
        self.post_json(
            "/v1/xdp/switch",
            &serde_json::json!({ "slot": slot, "generation": generation, "policy": policy }),
        )
        .await
    }

    /// `GET /v1/xdp/status`.
    pub async fn xdp_status(&self) -> Result<String> {
        self.get_message("/v1/xdp/status").await
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
    pub async fn update_map_batch(
        &self,
        name: &str,
        entries: &[(String, String)],
    ) -> Result<String> {
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
        self.post_json("/v1/clear/perf", &serde_json::json!({}))
            .await
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
        Ok(self.post_response(path, body).await?.message)
    }

    async fn post_response(&self, path: &str, body: &serde_json::Value) -> Result<ApiResp> {
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
        Ok(api)
    }
}

fn perf_field<'a>(reply: &'a str, name: &str) -> Result<&'a str> {
    reply
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
        .with_context(|| format!("missing {name} in perf reply"))
}

fn normalize_base(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if e.starts_with("http://") || e.starts_with("https://") {
        e.to_string()
    } else {
        format!("http://{e}")
    }
}

#[cfg(test)]
mod tests {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn medium_http_200_failure_preserves_the_preflight_issue() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let agent = super::AgentClient::new(listener.local_addr().unwrap().to_string());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 1024];
            let n = socket.read(&mut bytes).await.unwrap();
            assert!(bytes[..n].starts_with(b"GET /v1/preflight/medium HTTP/1.1\r\n"));
            let body = r#"{"ok":false,"preflight":{"release":"v0.1.1","required":32,"deployments":[],"issues":["case_mark_idle slot B layout mismatch"]}}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
        });
        let err = agent
            .medium_preflight()
            .await
            .err()
            .expect("business failure must block");
        assert!(
            err.to_string()
                .contains("case_mark_idle slot B layout mismatch")
        );
        server.await.unwrap();
    }
}
