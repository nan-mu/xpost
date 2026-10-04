//! JSON `/health` + `/v1/*` and MCP `/mcp` on one axum server.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Json;
use axum::extract::{Path as AxumPath, State};
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
use crate::ops::{self, MapKv};
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
struct FlashByNameBody {
    name: String,
}

#[derive(Deserialize)]
struct ClearXdpBody {
    /// Required: `"*"` | `"prog"` | `["a","b"]`.
    name: ClearXdpName,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ClearXdpName {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct UpdateMapBody {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    entries: Option<Vec<MapEntryBody>>,
}

#[derive(Deserialize)]
struct MapEntryBody {
    key: String,
    value: String,
}

#[derive(Deserialize)]
struct RunBody {
    cmd: String,
}

#[derive(Deserialize)]
struct SerialBody {
    since_unix_ms: u64,
    until_unix_ms: u64,
}

pub fn serve_http(addr: &str, log_dir: &Path) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(serve_async(addr, log_dir))
}

async fn serve_async(addr: &str, log_dir: &Path) -> Result<()> {
    let log_owned = log_dir.to_path_buf();
    let shared = tokio::task::spawn_blocking(move || open(&log_owned))
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
        .route("/get/init", get(v1_get_init))
        .route("/flash/fw", post(v1_flash_fw))
        .route("/flash/xdp", post(v1_flash_xdp))
        .route("/clear/xdp", post(v1_clear_xdp))
        .route("/get/xdp", get(v1_get_xdp))
        .route("/enable/xdp", post(v1_enable_xdp))
        .route("/disable/xdp", post(v1_disable_xdp))
        .route("/clear/map/{map_name}", post(v1_clear_map))
        .route("/get/map/{map_name}", get(v1_get_map))
        .route("/update/map/{map_name}", post(v1_update_map))
        .route("/clear/perf", post(v1_clear_perf))
        .route("/get/perf", get(v1_get_perf))
        .route("/get/serial", post(v1_get_serial))
        .route("/run", post(v1_run))
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
        "[{}] xpost-agent HTTP {addr}  (/health /v1/* /mcp)",
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

async fn v1_get_init(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::get_init(shared.as_ref(), "http")).await
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

async fn v1_flash_fw(
    State(st): State<AppState>,
    Json(body): Json<FlashByNameBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let name = body.name;
    run_blocking(move || ops::flash_fw(shared.as_ref(), &name, "http")).await
}

async fn v1_flash_xdp(
    State(st): State<AppState>,
    Json(body): Json<FlashByNameBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let name = body.name;
    run_blocking(move || ops::flash_xdp(shared.as_ref(), &name, "http")).await
}

fn clear_xdp_names(name: ClearXdpName) -> Result<Vec<String>, String> {
    match name {
        ClearXdpName::One(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Err("missing name".into());
            }
            if s == "*" {
                Ok(Vec::new())
            } else {
                Ok(vec![s.to_string()])
            }
        }
        ClearXdpName::Many(v) => {
            if v.is_empty() {
                return Err("missing name".into());
            }
            if v.len() == 1 && v[0].trim() == "*" {
                return Ok(Vec::new());
            }
            Ok(v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        }
    }
}

async fn v1_clear_xdp(
    State(st): State<AppState>,
    body: Result<Json<ClearXdpBody>, axum::extract::rejection::JsonRejection>,
) -> Json<ApiResp> {
    let Json(body) = match body {
        Ok(j) => j,
        Err(e) => return Json(ApiResp::err(format!("missing name: {e}"))),
    };
    let names = match clear_xdp_names(body.name) {
        Ok(n) => n,
        Err(e) => return Json(ApiResp::err(e)),
    };
    let shared = st.shared.clone();
    run_blocking(move || ops::clear_xdp(shared.as_ref(), names, "http")).await
}

async fn v1_get_xdp(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::get_xdp(shared.as_ref(), "http")).await
}

async fn v1_enable_xdp(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::enable_xdp(shared.as_ref(), "http")).await
}

async fn v1_disable_xdp(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::disable_xdp(shared.as_ref(), "http")).await
}

async fn v1_clear_map(
    State(st): State<AppState>,
    AxumPath(map_name): AxumPath<String>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::clear_map(shared.as_ref(), &map_name, "http")).await
}

async fn v1_get_map(
    State(st): State<AppState>,
    AxumPath(map_name): AxumPath<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let shared = st.shared.clone();
    match tokio::task::spawn_blocking(move || ops::get_map_json(shared.as_ref(), &map_name, "http"))
        .await
    {
        Ok(Ok(map)) => (StatusCode::OK, Json(map)).into_response(),
        Ok(Err(e)) => (StatusCode::OK, Json(ApiResp::err(format!("{e:#}")))).into_response(),
        Err(e) => (StatusCode::OK, Json(ApiResp::err(format!("join: {e}")))).into_response(),
    }
}

async fn v1_update_map(
    State(st): State<AppState>,
    AxumPath(map_name): AxumPath<String>,
    Json(body): Json<UpdateMapBody>,
) -> Json<ApiResp> {
    let entries = match (&body.key, &body.value, &body.entries) {
        (Some(k), Some(v), None) => vec![MapKv {
            key: k.clone(),
            value: v.clone(),
        }],
        (_, _, Some(list)) if !list.is_empty() => list
            .iter()
            .map(|e| MapKv {
                key: e.key.clone(),
                value: e.value.clone(),
            })
            .collect(),
        _ => {
            return Json(ApiResp::err(
                "body must be {\"key\",\"value\"} or {\"entries\":[…]}",
            ));
        }
    };
    let shared = st.shared.clone();
    run_blocking(move || ops::update_map(shared.as_ref(), &map_name, &entries, "http")).await
}

async fn v1_clear_perf(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::clear_perf(shared.as_ref(), "http")).await
}

async fn v1_get_perf(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    run_blocking(move || ops::get_perf(shared.as_ref(), "http")).await
}

async fn v1_get_serial(State(st): State<AppState>, Json(body): Json<SerialBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let since = body.since_unix_ms;
    let until = body.until_unix_ms;
    run_blocking(move || ops::get_serial(shared.as_ref(), since, until, "http")).await
}

async fn v1_run(State(st): State<AppState>, Json(body): Json<RunBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let cmd = body.cmd;
    run_blocking(move || ops::run_cmd(shared.as_ref(), &cmd, "http")).await
}
