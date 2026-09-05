//! Length-prefixed RPC dispatch for the Mac flash agent.

use std::sync::Arc;

use anyhow::{Context, Result};
use test_common::fw_agent::{
    self, AGENT_ERROR, AGENT_FLASH_PROFILE, AGENT_FLASHING, AGENT_GET_STATUS, AGENT_IDLE,
    AGENT_LIST_PROFILES, AGENT_REBOOT, FlashProfileResp, GetStatusResp, ListProfilesResp,
    ProfileInfo, RebootResp, STATUS_BAD, STATUS_OK,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::profiles::{Profile, find_profile};
use crate::serial::SerialSupervisor;

#[derive(Clone)]
pub struct AgentState {
    inner: Arc<Mutex<State>>,
    profiles: Vec<Profile>,
    serial: Arc<SerialSupervisor>,
}

struct State {
    phase: u32,
    last_profile_id: String,
    message: String,
}

impl AgentState {
    pub fn new(profiles: Vec<Profile>, serial: Arc<SerialSupervisor>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                phase: AGENT_IDLE,
                last_profile_id: String::new(),
                message: "idle; awaiting FlashProfile".to_string(),
            })),
            profiles,
            serial,
        }
    }

    pub async fn handle_connection(&self, mut stream: TcpStream) -> Result<()> {
        loop {
            let mut hdr = [0u8; 8];
            match stream.read_exact(&mut hdr).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e).context("read rpc header"),
            }
            let method = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
            let mut body = vec![0u8; len];
            if len > 0 {
                stream
                    .read_exact(&mut body)
                    .await
                    .context("read rpc body")?;
            }
            let (resp_method, resp_body) = self.dispatch(method, &body).await?;
            let mut out_hdr = [0u8; 8];
            out_hdr[0..4].copy_from_slice(&resp_method.to_le_bytes());
            out_hdr[4..8].copy_from_slice(&(resp_body.len() as u32).to_le_bytes());
            stream.write_all(&out_hdr).await?;
            if !resp_body.is_empty() {
                stream.write_all(&resp_body).await?;
            }
            stream.flush().await?;
        }
    }

    async fn dispatch(&self, method: u32, body: &[u8]) -> Result<(u32, Vec<u8>)> {
        match method {
            AGENT_LIST_PROFILES => {
                let profiles: Vec<ProfileInfo> = self
                    .profiles
                    .iter()
                    .map(|p| ProfileInfo {
                        profile_id: p.id.clone(),
                        path: match p.path.as_str() {
                            "xdp_drop" => fw_agent::DROP_PATH_XDP_DROP,
                            "hal_drop" => fw_agent::DROP_PATH_HAL_DROP,
                            other => {
                                log::warn!("unknown profile path tag `{other}`");
                                0
                            }
                        },
                        bin_path: p.bin.to_string_lossy().into_owned(),
                    })
                    .collect();
                let resp = ListProfilesResp { profiles };
                let mut buf = [0u8; 4096];
                let n = fw_agent::encode_list_profiles_resp(&resp, &mut buf)
                    .context("encode list profiles")?;
                Ok((AGENT_LIST_PROFILES, buf[..n].to_vec()))
            }
            AGENT_FLASH_PROFILE => {
                let id_bytes = fw_agent::decode_flash_profile_req(body)
                    .map_err(|e| anyhow::anyhow!("decode flash req: {:?}", e))?;
                let id = std::str::from_utf8(id_bytes).context("profile id utf8")?;
                let profile = find_profile(&self.profiles, id)?;
                {
                    let mut st = self.inner.lock().await;
                    st.phase = AGENT_FLASHING;
                    st.message = format!("flashing {} — {}", profile.id, profile.description);
                }
                let result = self.serial.flash_profile(profile).await;
                let (status, message) = match result {
                    Ok(()) => (STATUS_OK, format!("flashed {id}; monitor resumed")),
                    Err(e) => (STATUS_BAD, format!("{e:#}")),
                };
                let mut st = self.inner.lock().await;
                st.phase = if status == STATUS_OK {
                    AGENT_IDLE
                } else {
                    AGENT_ERROR
                };
                st.last_profile_id = id.to_string();
                st.message = message.clone();
                let resp = FlashProfileResp { status, message };
                let mut buf = [0u8; 512];
                let n = fw_agent::encode_flash_profile_resp(&resp, &mut buf)
                    .context("encode flash profile resp")?;
                Ok((AGENT_FLASH_PROFILE, buf[..n].to_vec()))
            }
            AGENT_REBOOT => {
                let result = self.serial.reboot_device().await;
                let (status, message) = match result {
                    Ok(()) => (STATUS_OK, "rebooted; monitor resumed".to_string()),
                    Err(e) => (STATUS_BAD, format!("{e:#}")),
                };
                let resp = RebootResp { status, message };
                let mut buf = [0u8; 256];
                let n =
                    fw_agent::encode_reboot_resp(&resp, &mut buf).context("encode reboot resp")?;
                Ok((AGENT_REBOOT, buf[..n].to_vec()))
            }
            AGENT_GET_STATUS => {
                let st = self.inner.lock().await;
                let esp_link_ok = st.phase == AGENT_IDLE && self.serial.monitor_active().await;
                let resp = GetStatusResp {
                    state: st.phase,
                    last_profile_id: st.last_profile_id.clone(),
                    esp_link_ok,
                    message: st.message.clone(),
                };
                let mut buf = [0u8; 512];
                let n = fw_agent::encode_get_status_resp(&resp, &mut buf)
                    .context("encode get status resp")?;
                Ok((AGENT_GET_STATUS, buf[..n].to_vec()))
            }
            _ => {
                let resp = FlashProfileResp {
                    status: STATUS_BAD,
                    message: "unknown method".into(),
                };
                let mut buf = [0u8; 128];
                let n = fw_agent::encode_flash_profile_resp(&resp, &mut buf)
                    .context("encode flash profile resp")?;
                Ok((method, buf[..n].to_vec()))
            }
        }
    }
}
