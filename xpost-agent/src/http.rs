//! JSON `/health` + `/v1/*` and MCP `/mcp` on one axum server.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Router, serve};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};
use serde::{Deserialize, Serialize};
use tower_http::timeout::TimeoutLayer;

use crate::mcp::XpostMcp;
use crate::ops;
use crate::state::{SharedState, open};
use crate::util::{eprint_line, ts};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1200);

#[derive(Clone)]
struct AppState {
    shared: SharedState,
}

#[derive(Serialize)]
struct ApiResp {
    ok: bool,
    message: String,
}

impl ApiResp {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
        }
    }
    fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            message: e.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct FlashBody {
    profile_id: String,
}

#[derive(Deserialize)]
struct ProgramBody {
    elf: String,
}

#[derive(Deserialize)]
struct RequestBody {
    cmd: String,
}

#[derive(Deserialize)]
struct ProbeBody {
    esp: String,
    #[serde(default = "default_probe_port")]
    port: u16,
}

#[derive(Deserialize)]
struct ProbeBurstBody {
    esp: String,
    #[serde(default = "default_probe_port")]
    port: u16,
    count: u32,
}

#[derive(Deserialize)]
struct SerialWindowBody {
    /// Inclusive start, unix epoch milliseconds (host clock).
    since_unix_ms: u64,
    /// Inclusive end, unix epoch milliseconds (host clock).
    until_unix_ms: u64,
}

fn default_probe_port() -> u16 {
    8080
}

pub fn serve_http(addr: &str, port: Option<&Path>, log: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(serve_async(addr, port, log))
}

async fn serve_async(addr: &str, port: Option<&Path>, log: &Path) -> Result<()> {
    let port_owned = port.map(|p| p.to_path_buf());
    let log_owned = log.to_path_buf();
    let shared = tokio::task::spawn_blocking(move || open(port_owned.as_deref(), &log_owned))
        .await
        .context("join open session")?
        .context("open session")?;

    let app_state = AppState {
        shared: shared.clone(),
    };

    let mcp_shared = shared.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(XpostMcp::new(mcp_shared.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );

    let v1 = Router::new()
        .route("/status", get(v1_status))
        .route("/dut_status", get(v1_dut_status))
        .route("/flash_profile", post(v1_flash_profile))
        .route("/program", post(v1_program))
        .route("/clear", post(v1_clear))
        .route("/list", get(v1_list))
        .route("/counters", post(v1_counters))
        .route("/request", post(v1_request))
        .route("/serial_window", post(v1_serial_window))
        .route("/probe", post(v1_probe))
        .route("/probe_burst", post(v1_probe_burst))
        .with_state(app_state);

    let app = Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .nest_service("/mcp", mcp_service)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    eprint_line(&format!(
        "[{}] xpost-agent HTTP {addr}  (/health /v1/* /mcp incl. /v1/dut_status)",
        ts()
    ));
    serve(listener, app).await.context("axum serve")?;
    Ok(())
}

async fn health() -> Json<ApiResp> {
    Json(ApiResp::ok("ok"))
}

async fn v1_status() -> Json<ApiResp> {
    Json(ApiResp::ok(ops::get_status()))
}

async fn v1_dut_status(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::dut_status(shared.as_ref())).await
}

async fn run_blocking<F>(f: F) -> Json<ApiResp>
where
    F: FnOnce() -> anyhow::Result<String> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(msg)) => Json(ApiResp::ok(msg)),
        Ok(Err(e)) => Json(ApiResp::err(format!("{e:#}"))),
        Err(e) => Json(ApiResp::err(format!("join: {e}"))),
    }
}

async fn v1_flash_profile(
    State(st): State<AppState>,
    Json(body): Json<FlashBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let id = body.profile_id;
    run_blocking(move || ops::flash_profile(shared.as_ref(), &id, "http")).await
}

async fn v1_program(State(st): State<AppState>, Json(body): Json<ProgramBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let elf = body.elf;
    run_blocking(move || ops::program(shared.as_ref(), &elf, "http")).await
}

async fn v1_clear(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::clear_xdp(shared.as_ref(), "http")).await
}

async fn v1_list(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::list_xdp(shared.as_ref(), "http")).await
}

async fn v1_counters(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::counters(shared.as_ref(), "http")).await
}

async fn v1_request(State(st): State<AppState>, Json(body): Json<RequestBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let cmd = body.cmd;
    run_blocking(move || ops::request(shared.as_ref(), &cmd, "http")).await
}

async fn v1_serial_window(
    State(st): State<AppState>,
    Json(body): Json<SerialWindowBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let since = body.since_unix_ms;
    let until = body.until_unix_ms;
    run_blocking(move || ops::serial_window(shared.as_ref(), since, until, "http")).await
}

async fn v1_probe(Json(body): Json<ProbeBody>) -> Json<ApiResp> {
    let esp = body.esp;
    let port = body.port;
    run_blocking(move || ops::probe(&esp, port, "http")).await
}

async fn v1_probe_burst(Json(body): Json<ProbeBurstBody>) -> Json<ApiResp> {
    let esp = body.esp;
    let port = body.port;
    let count = body.count;
    run_blocking(move || ops::probe_burst_op(&esp, port, count, "http")).await
}
