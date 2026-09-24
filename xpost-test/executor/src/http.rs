//! HTTP control plane: submit jobs, poll status, stream captured stdout/stderr.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use test_common::fw_agent::{JOB_FAILED};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

use crate::jobs::is_terminal;
use crate::run::{run_test_job, validate_bench};
use crate::rpc::RpcState;

#[derive(Clone)]
pub struct HttpState {
    pub rpc: RpcState,
}

#[derive(Debug, Deserialize)]
pub struct SubmitBody {
    pub bench: String,
    #[serde(default)]
    pub smoke: bool,
    #[serde(default)]
    pub formal: bool,
}

#[derive(Debug, Serialize)]
pub struct SubmitReply {
    pub job_id: u64,
    pub status: &'static str,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct JobReply {
    pub job_id: u64,
    pub state: u32,
    pub terminal: bool,
    pub bench: String,
    pub message: String,
    pub log_bytes: usize,
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub offset: usize,
}

#[derive(Debug, Serialize)]
pub struct LogReply {
    pub offset: usize,
    pub next_offset: usize,
    pub chunk: String,
    pub eof: bool,
}

pub async fn serve(listen: &str, state: HttpState) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/api/jobs", post(submit_job))
        .route("/api/jobs/:id", get(get_job))
        .route("/api/jobs/:id/log", get(get_job_log))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(state);

    let listener = TcpListener::bind(listen).await?;
    eprintln!("executor HTTP on {listen}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn submit_job(
    State(st): State<HttpState>,
    Json(body): Json<SubmitBody>,
) -> Result<Json<SubmitReply>, StatusCode> {
    validate_bench(&body.bench).map_err(|e| {
        eprintln!("submit reject: {e:#}");
        StatusCode::BAD_REQUEST
    })?;

    let job = st
        .rpc
        .jobs
        .create(body.bench.clone(), String::new(), body.smoke, body.formal)
        .await;
    let job_id = job.id;
    let log = st.rpc.jobs.log_for(job_id).await;
    let rpc = st.rpc.clone();
    tokio::spawn(async move {
        if let Err(e) = run_test_job(&rpc.jobs, job_id, &rpc.cfg, log).await {
            eprintln!("job {job_id} error: {e:#}");
            rpc.jobs
                .set_state(job_id, JOB_FAILED, format!("{e:#}"))
                .await;
        }
    });

    Ok(Json(SubmitReply {
        job_id,
        status: "accepted",
        message: format!("bench={}", body.bench),
    }))
}

async fn get_job(
    State(st): State<HttpState>,
    Path(id): Path<u64>,
) -> Result<Json<JobReply>, StatusCode> {
    let job = st.rpc.jobs.get(id).await.ok_or(StatusCode::NOT_FOUND)?;
    let log_bytes = st.rpc.jobs.log_for(id).await.len().await;
    Ok(Json(JobReply {
        job_id: id,
        state: job.state,
        terminal: is_terminal(job.state),
        bench: job.bench,
        message: job.message,
        log_bytes,
    }))
}

async fn get_job_log(
    State(st): State<HttpState>,
    Path(id): Path<u64>,
    Query(q): Query<LogQuery>,
) -> Result<Json<LogReply>, StatusCode> {
    if st.rpc.jobs.get(id).await.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let log = st.rpc.jobs.log_for(id).await;
    let (off, chunk) = log.read_from(q.offset).await;
    let job = st.rpc.jobs.get(id).await.unwrap();
    let eof = is_terminal(job.state);
    let text = String::from_utf8_lossy(&chunk).into_owned();
    Ok(Json(LogReply {
        offset: off,
        next_offset: off + chunk.len(),
        chunk: text,
        eof,
    }))
}
