//! Executor RPC server (SubmitJob / GetJob / CancelJob).

use std::sync::Arc;

use anyhow::{Context, Result};
use test_common::fw_agent::{
    self, EXEC_CANCEL_JOB, EXEC_GET_JOB, EXEC_SUBMIT_JOB, JOB_FAILED, STATUS_BAD, STATUS_OK,
    CancelJobResp, GetJobResp, SubmitJobResp,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::agent_client::AgentClient;
use crate::config::ExecutorConfig;
use crate::jobs::Jobs;
use crate::run::{run_test_job, validate_bench};

#[derive(Clone)]
pub struct RpcState {
    pub jobs: Arc<Jobs>,
    pub agent: AgentClient,
    pub cfg: ExecutorConfig,
}

impl RpcState {
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
            EXEC_SUBMIT_JOB => {
                let (bench, smoke, formal, profile_id) =
                    fw_agent::decode_submit_job_req(body)
                        .map_err(|e| anyhow::anyhow!("decode SubmitJobReq: {:?}", e))?;
                if bench.is_empty() {
                    let resp = SubmitJobResp {
                        status: STATUS_BAD,
                        job_id: 0,
                        message: "bench required".into(),
                    };
                    let mut buf = [0u8; 128];
                    let n = fw_agent::encode_submit_job_resp(&resp, &mut buf)
                        .context("encode submit job resp")?;
                    return Ok((EXEC_SUBMIT_JOB, buf[..n].to_vec()));
                }
                if let Err(e) = validate_bench(&bench) {
                    let resp = SubmitJobResp {
                        status: STATUS_BAD,
                        job_id: 0,
                        message: format!("{e:#}"),
                    };
                    let mut buf = [0u8; 256];
                    let n = fw_agent::encode_submit_job_resp(&resp, &mut buf)
                        .context("encode submit job resp")?;
                    return Ok((EXEC_SUBMIT_JOB, buf[..n].to_vec()));
                }
                let job = self
                    .jobs
                    .create(bench.clone(), profile_id, smoke, formal)
                    .await;
                let job_id = job.id;
                let log = self.jobs.log_for(job_id).await;
                let state = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_test_job(&state.jobs, job_id, &state.cfg, log).await {
                        log::error!("job {job_id} failed: {e:#}");
                        state
                            .jobs
                            .set_state(job_id, JOB_FAILED, format!("{e:#}"))
                            .await;
                    }
                });
                let resp = SubmitJobResp {
                    status: STATUS_OK,
                    job_id,
                    message: "accepted".into(),
                };
                let mut buf = [0u8; 256];
                let n = fw_agent::encode_submit_job_resp(&resp, &mut buf)
                    .context("encode submit job resp")?;
                Ok((EXEC_SUBMIT_JOB, buf[..n].to_vec()))
            }
            EXEC_GET_JOB => {
                let job_id = fw_agent::decode_get_job_req(body)
                    .map_err(|e| anyhow::anyhow!("decode GetJobReq: {:?}", e))?;
                let job = self.jobs.get(job_id).await;
                let (status, job_state, message, bench, profile_id) = match job {
                    Some(j) => (
                        STATUS_OK,
                        j.state,
                        j.message.clone(),
                        j.bench.clone(),
                        j.profile_id.clone(),
                    ),
                    None => (
                        STATUS_BAD,
                        0,
                        "unknown job".to_string(),
                        String::new(),
                        String::new(),
                    ),
                };
                let resp = GetJobResp {
                    status,
                    job_state,
                    message,
                    bench,
                    profile_id,
                };
                let mut buf = [0u8; 512];
                let n = fw_agent::encode_get_job_resp(&resp, &mut buf)
                    .context("encode get job resp")?;
                Ok((EXEC_GET_JOB, buf[..n].to_vec()))
            }
            EXEC_CANCEL_JOB => {
                let job_id = fw_agent::decode_cancel_job_req(body)
                    .map_err(|e| anyhow::anyhow!("decode CancelJobReq: {:?}", e))?;
                let ok = self.jobs.cancel(job_id).await;
                let resp = CancelJobResp {
                    status: if ok { STATUS_OK } else { STATUS_BAD },
                    message: if ok {
                        "cancelled".into()
                    } else {
                        "not cancellable".into()
                    },
                };
                let mut buf = [0u8; 128];
                let n = fw_agent::encode_cancel_job_resp(&resp, &mut buf)
                    .context("encode cancel job resp")?;
                Ok((EXEC_CANCEL_JOB, buf[..n].to_vec()))
            }
            _ => {
                let resp = SubmitJobResp {
                    status: STATUS_BAD,
                    job_id: 0,
                    message: "unknown method".into(),
                };
                let mut buf = [0u8; 128];
                let n = fw_agent::encode_submit_job_resp(&resp, &mut buf)
                    .context("encode submit job resp")?;
                Ok((method, buf[..n].to_vec()))
            }
        }
    }
}
