//! Mac flash agent TCP client (8-byte method+len header, protobuf body).

use anyhow::{Context, Result, bail};
use test_common::fw_agent::{
    self, AGENT_FLASH_PROFILE, AGENT_GET_STATUS, AGENT_IDLE, AGENT_REBOOT, STATUS_OK,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

#[derive(Clone)]
pub struct AgentClient {
    endpoint: String,
}

impl AgentClient {
    pub fn new(endpoint: String) -> Self {
        Self { endpoint }
    }

    pub async fn flash_profile(&self, profile_id: &str) -> Result<()> {
        let mut body = [0u8; 128];
        let n = fw_agent::encode_flash_profile_req(profile_id, &mut body)
            .context("encode FlashProfileReq")?;
        let resp = self.call(AGENT_FLASH_PROFILE, &body[..n]).await?;
        let flash = fw_agent::decode_flash_profile_resp(&resp)
            .map_err(|e| anyhow::anyhow!("decode FlashProfileResp: {:?}", e))?;
        if flash.status != STATUS_OK {
            bail!("FlashProfile: {}", flash.message);
        }
        Ok(())
    }

    /// Reserved for future executor flows (post-flash MCU reset).
    #[allow(dead_code)]
    pub async fn reboot(&self) -> Result<()> {
        let resp = self.call(AGENT_REBOOT, &[]).await?;
        let rb = fw_agent::decode_reboot_resp(&resp)
            .map_err(|e| anyhow::anyhow!("decode RebootResp: {:?}", e))?;
        if rb.status != STATUS_OK {
            bail!("Reboot: {}", rb.message);
        }
        Ok(())
    }

    pub async fn wait_idle(&self, budget: Duration) -> Result<()> {
        let start = tokio::time::Instant::now();
        while start.elapsed() < budget {
            let resp = self.call(AGENT_GET_STATUS, &[]).await?;
            let st = fw_agent::decode_get_status_resp(&resp)
                .map_err(|e| anyhow::anyhow!("decode GetStatusResp: {:?}", e))?;
            if st.state == AGENT_IDLE {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("agent not idle within {:?}", budget);
    }

    async fn call(&self, method: u32, body: &[u8]) -> Result<Vec<u8>> {
        let mut stream = timeout(
            Duration::from_secs(10),
            TcpStream::connect(&self.endpoint),
        )
        .await
        .context("connect agent timeout")?
        .with_context(|| format!("connect agent {}", self.endpoint))?;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&method.to_le_bytes());
        hdr[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
        stream.write_all(&hdr).await?;
        if !body.is_empty() {
            stream.write_all(body).await?;
        }
        stream.flush().await?;

        stream
            .read_exact(&mut hdr)
            .await
            .context("read rpc response header")?;
        let resp_method = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
        if resp_method != method {
            log::warn!("rpc response method {resp_method} != request {method}");
        }
        let mut resp_body = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut resp_body).await?;
        }
        Ok(resp_body)
    }
}
