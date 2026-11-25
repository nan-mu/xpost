//! HTTP routes and the device-operation admission boundary.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, serve};
use serde::{Deserialize, Serialize};
use tower_http::timeout::TimeoutLayer;

use crate::config::Location;
use crate::ops::{self, MapKv};
use crate::profiles::Profiles;
use crate::state;
use crate::state::{SharedState, open};
use crate::util::{eprint_line, ts};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1200);

#[derive(Clone)]
struct AppState {
    shared: SharedState,
    control: Arc<Control>,
    host: Arc<HostConfig>,
}

struct HostConfig {
    root: std::path::PathBuf,
    profiles: Profiles,
}

#[derive(Default)]
struct Control {
    activity: Mutex<Activity>,
    device: Arc<tokio::sync::Mutex<()>>,
    drained: tokio::sync::Notify,
}

#[derive(Default)]
struct Activity {
    stopping: bool,
    pending: usize,
}

struct Operation(Arc<Control>);

impl Drop for Operation {
    fn drop(&mut self) {
        let mut activity = self.0.activity.lock().unwrap();
        activity.pending -= 1;
        if activity.pending == 0 {
            self.0.drained.notify_one();
        }
    }
}

impl Control {
    fn admit(self: &Arc<Self>) -> Result<Operation> {
        let mut activity = self.activity.lock().unwrap();
        anyhow::ensure!(!activity.stopping, "Agent is stopping");
        activity.pending += 1;
        Ok(Operation(self.clone()))
    }

    fn stop(&self) {
        self.activity.lock().unwrap().stopping = true;
    }

    fn status(&self) -> &'static str {
        let activity = self.activity.lock().unwrap();
        if activity.stopping {
            "stopping"
        } else if activity.pending > 0 {
            "busy"
        } else {
            "idle"
        }
    }

    async fn drain(&self) {
        while self.activity.lock().unwrap().pending > 0 {
            self.drained.notified().await;
        }
    }
}

impl AppState {
    async fn execute<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let operation = self.control.admit()?;
        // This task owns the work even if the HTTP future is dropped.
        tokio::spawn(async move {
            let control = operation.0.clone();
            let device = control.device.clone().lock_owned().await;
            tokio::task::spawn_blocking(move || {
                let _operation = operation;
                let _device = device;
                f()
            })
            .await
            .context("join device operation")?
        })
        .await
        .context("join admitted operation")?
    }

    async fn reply<T: Into<ops::OpReply> + Send + 'static>(
        &self,
        f: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Json<ApiResp> {
        match self.execute(f).await {
            Ok(reply) => {
                let reply = reply.into();
                Json(ApiResp {
                    ok: true,
                    message: reply.message,
                    flash: reply.flash,
                })
            }
            Err(error) => Json(ApiResp::err(format!("{error:#}"))),
        }
    }
}

#[derive(Serialize)]
struct ApiResp {
    ok: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    flash: Option<ops::FlashIdentity>,
}

impl ApiResp {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            flash: None,
        }
    }
    fn err(e: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            message: e.to_string(),
            flash: None,
        }
    }
}

#[derive(Deserialize)]
struct FlashByNameBody {
    name: String,
    expected_sha256: Option<String>,
}

#[derive(Deserialize)]
struct FlashXdpBody {
    name: String,
    #[serde(default)]
    slot: Option<u8>,
    firmware: Option<String>,
    expected_sha256: Option<String>,
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

pub fn serve_http(addr: &str, log_dir: &Path, location: Location) -> Result<()> {
    let root = location.root()?;
    let profiles = state::load_profiles(&root)?;
    std::fs::create_dir_all(log_dir)?;
    let log_dir = log_dir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _device_lock = crate::device_lock::claim()?;
    let result = runtime.block_on(serve_async(
        addr,
        &log_dir,
        location.worktree_id(),
        root,
        profiles,
    ));
    drop(runtime);
    result
}

async fn serve_async(
    addr: &str,
    log_dir: &Path,
    worktree: String,
    root: std::path::PathBuf,
    profiles: Profiles,
) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let host = Arc::new(HostConfig {
        root: root.clone(),
        profiles: profiles.clone(),
    });
    let log_dir = log_dir.to_owned();
    let shared = tokio::task::spawn_blocking(move || open(root, profiles, &log_dir))
        .await
        .context("join device initialization")??;
    let control = Arc::new(Control::default());
    let app_state = AppState {
        shared: shared.clone(),
        control: control.clone(),
        host,
    };
    let v1 = Router::new()
        .route("/status", get(v1_status))
        .route("/dut_status", get(v1_dut_status))
        .route("/preflight/medium", get(v1_medium_preflight))
        .route("/preflight/medium/{stage}", get(v1_stage_preflight))
        .route("/deployment/attest", post(v1_attest_deployment))
        .route("/preflight/xdp/{name}", get(v1_xdp_preflight))
        .route("/get/init", get(v1_get_init))
        .route("/flash/fw", post(v1_flash_fw))
        .route("/flash/xdp", post(v1_flash_xdp))
        .route("/clear/xdp", post(v1_clear_xdp))
        .route("/get/xdp", get(v1_get_xdp))
        .route("/enable/xdp", post(v1_enable_xdp))
        .route("/disable/xdp", post(v1_disable_xdp))
        .route("/xdp/switch", post(v1_xdp_switch))
        .route("/xdp/status", get(v1_xdp_status))
        .route("/enable/exec", post(v1_enable_exec))
        .route("/exec/batch", post(v1_exec_batch))
        .route("/disable/exec", post(v1_disable_exec))
        .route("/get/finish", post(v1_get_finish))
        .route("/clear/map/{map_name}", post(v1_clear_map))
        .route("/get/map/{map_name}", get(v1_get_map))
        .route("/update/map/{map_name}", post(v1_update_map))
        .route("/clear/perf", post(v1_clear_perf))
        .route("/get/perf", get(v1_get_perf))
        .route("/perf/prepare", post(v1_perf_prepare))
        .route("/perf/arm", post(v1_perf_arm))
        .route("/perf/freeze", post(v1_perf_freeze))
        .route("/perf/read", post(v1_perf_read))
        .route("/perf/release", post(v1_perf_release))
        .route("/get/serial", post(v1_get_serial))
        .route("/run", post(v1_run))
        .with_state(app_state);

    let app = Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .layer(middleware::from_fn_with_state(
            HeaderValue::from_str(&worktree)?,
            check_worktree,
        ))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout(),
        ));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    eprint_line(&format!(
        "[{}] xpost-agent HTTP {}",
        ts(),
        listener.local_addr()?
    ));
    let shutdown = control.clone();
    let served = serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            shutdown.stop();
        })
        .await;
    control.stop();
    control.drain().await;
    let state = Arc::try_unwrap(shared)
        .map_err(|_| anyhow::anyhow!("device state still shared at shutdown"))?
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    tokio::task::spawn_blocking(move || state.handler.shutdown()).await?;
    served.context("serve Agent HTTP")
}

fn request_timeout() -> Duration {
    #[cfg(feature = "test-device")]
    if let Some(timeout) = crate::fixture::request_timeout() {
        return timeout;
    }
    REQUEST_TIMEOUT
}

pub const WORKTREE_HEADER: &str = "x-xpost-worktree";

async fn check_worktree(State(worktree): State<HeaderValue>, req: Request, next: Next) -> Response {
    if req
        .headers()
        .get(WORKTREE_HEADER)
        .is_some_and(|id| id != worktree)
    {
        return (
            StatusCode::CONFLICT,
            Json(ApiResp::err("Agent belongs to another Worktree")),
        )
            .into_response();
    }
    let mut response = next.run(req).await;
    response.headers_mut().insert(WORKTREE_HEADER, worktree);
    response
}

async fn health() -> Json<ApiResp> {
    Json(ApiResp::ok("ok"))
}

async fn v1_status(State(st): State<AppState>) -> Json<ApiResp> {
    Json(ApiResp::ok(st.control.status()))
}

#[derive(Deserialize)]
struct ExecBatchBody {
    input_hex: String,
}

async fn v1_exec_batch(
    State(st): State<AppState>,
    Json(body): Json<ExecBatchBody>,
) -> Json<serde_json::Value> {
    let shared = st.shared.clone();
    match st
        .execute(move || ops::exec_batch(shared.as_ref(), &body.input_hex))
        .await
    {
        Ok(result) => Json(serde_json::json!({ "ok": true, "result": result })),
        Err(error) => Json(serde_json::json!({ "ok": false, "message": format!("{error:#}") })),
    }
}

async fn v1_medium_preflight(State(st): State<AppState>) -> Json<serde_json::Value> {
    medium_preflight(st, crate::preflight::Scope::All).await
}

async fn v1_stage_preflight(
    State(st): State<AppState>,
    AxumPath(stage): AxumPath<crate::preflight::Scope>,
) -> Json<serde_json::Value> {
    medium_preflight(st, stage).await
}

async fn medium_preflight(st: AppState, scope: crate::preflight::Scope) -> Json<serde_json::Value> {
    let host = st.host;
    match tokio::task::spawn_blocking(move || {
        crate::preflight::medium_scope(&host.root, &host.profiles, scope)
    })
    .await
    {
        Ok(report) => Json(serde_json::json!({
            "ok": report.deployment_ready(),
            "preflight": report,
        })),
        Err(error) => Json(serde_json::json!({ "ok": false, "message": format!("join: {error}") })),
    }
}

#[derive(Deserialize)]
struct AttestBody {
    firmware: String,
    expected_sha256: String,
}

async fn v1_attest_deployment(
    State(st): State<AppState>,
    Json(body): Json<AttestBody>,
) -> Json<serde_json::Value> {
    let shared = st.shared.clone();
    match st
        .execute(move || ops::attest_deployment(&shared, &body.firmware, &body.expected_sha256))
        .await
    {
        Ok(result) => Json(serde_json::json!({ "ok": true, "result": result })),
        Err(error) => Json(serde_json::json!({ "ok": false, "message": format!("{error:#}") })),
    }
}

async fn v1_dut_status(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::dut_status(shared.as_ref())).await
}

async fn v1_xdp_preflight(
    State(st): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> Json<ApiResp> {
    let host = st.host;
    match tokio::task::spawn_blocking(move || {
        crate::preflight::profile(&host.root, &host.profiles, &name)
    })
    .await
    {
        Ok(Ok(report)) => Json(ApiResp::ok(report.container_sha256)),
        Ok(Err(error)) => Json(ApiResp::err(format!("{error:#}"))),
        Err(error) => Json(ApiResp::err(format!("join: {error}"))),
    }
}

async fn v1_get_init(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::get_init(shared.as_ref(), "http"))
        .await
}

async fn v1_flash_fw(
    State(st): State<AppState>,
    Json(body): Json<FlashByNameBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let name = body.name;
    st.reply(move || {
        ops::flash_fw_checked(
            shared.as_ref(),
            &name,
            body.expected_sha256.as_deref(),
            "http",
        )
    })
    .await
}

async fn v1_flash_xdp(State(st): State<AppState>, Json(body): Json<FlashXdpBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let name = body.name;
    let slot = body.slot;
    st.reply(move || {
        let planned = match (body.firmware.as_deref(), body.expected_sha256.as_deref()) {
            (Some(firmware), Some(sha256)) => Some(ops::XdpTarget { firmware, sha256 }),
            (None, None) => None,
            _ => bail!("firmware and expected_sha256 must be provided together"),
        };
        ops::flash_xdp_checked(shared.as_ref(), &name, slot, planned, "http")
    })
    .await
}

fn clear_xdp_names(name: ClearXdpName) -> Result<Vec<String>, String> {
    let names = match name {
        ClearXdpName::One(name) => vec![name],
        ClearXdpName::Many(names) => names,
    };
    if names.len() == 1 && names[0] == "*" {
        return Ok(Vec::new());
    }
    if names.is_empty() || names.iter().any(|name| matches!(name.trim(), "" | "*")) {
        return Err("provide nonblank names or one exact '*'".into());
    }
    Ok(names
        .into_iter()
        .map(|name| name.trim().to_owned())
        .collect())
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
    st.reply(move || ops::clear_xdp(shared.as_ref(), names, "http"))
        .await
}

async fn v1_get_xdp(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::get_xdp(shared.as_ref(), "http"))
        .await
}

#[derive(Deserialize)]
struct ActivateBody {
    slot: u8,
    generation: u32,
    policy: String,
}

fn policy_pass(policy: &str) -> Result<bool> {
    match policy {
        "pass" => Ok(true),
        "drop" => Ok(false),
        _ => bail!("policy must be drop or pass"),
    }
}

async fn v1_xdp_switch(
    State(st): State<AppState>,
    body: Result<Json<ActivateBody>, axum::extract::rejection::JsonRejection>,
) -> Json<ApiResp> {
    let Json(body) = match body {
        Ok(j) => j,
        Err(e) => return Json(ApiResp::err(format!("bad switch body: {e}"))),
    };
    let pass = match policy_pass(&body.policy) {
        Ok(pass) => pass,
        Err(e) => return Json(ApiResp::err(e.to_string())),
    };
    let shared = st.shared.clone();
    st.reply(move || ops::switch_xdp(shared.as_ref(), body.slot, body.generation, pass, "http"))
        .await
}

async fn v1_xdp_status(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::xdp_status(shared.as_ref(), "http"))
        .await
}

async fn v1_enable_xdp(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::enable_xdp(shared.as_ref(), "http"))
        .await
}

async fn v1_disable_xdp(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::disable_xdp(shared.as_ref(), "http"))
        .await
}

async fn v1_enable_exec(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::enable_exec(shared.as_ref(), "http"))
        .await
}

async fn v1_disable_exec(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::disable_exec(shared.as_ref(), "http"))
        .await
}

async fn v1_get_finish(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::get_finish(shared.as_ref(), "http"))
        .await
}

async fn v1_clear_map(
    State(st): State<AppState>,
    AxumPath(map_name): AxumPath<String>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::clear_map(shared.as_ref(), &map_name, "http"))
        .await
}

async fn v1_get_map(
    State(st): State<AppState>,
    AxumPath(map_name): AxumPath<String>,
) -> axum::response::Response {
    let shared = st.shared.clone();
    match st
        .execute(move || ops::get_map_json(shared.as_ref(), &map_name, "http"))
        .await
    {
        Ok(map) => Json(map).into_response(),
        Err(error) => Json(ApiResp::err(format!("{error:#}"))).into_response(),
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
    st.reply(move || ops::update_map(shared.as_ref(), &map_name, &entries, "http"))
        .await
}

async fn v1_clear_perf(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::clear_perf(shared.as_ref(), "http"))
        .await
}

async fn v1_get_perf(State(st): State<AppState>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::get_perf(shared.as_ref(), "http"))
        .await
}

#[derive(Deserialize)]
struct PerfPrepareBody {
    uuid: String,
    variant: String,
    profile: String,
}

#[derive(Deserialize)]
struct PerfEpochBody {
    epoch: u32,
}

#[derive(Deserialize)]
struct PerfReadBody {
    epoch: u32,
    offset: u32,
    max_bytes: u32,
}

async fn v1_perf_prepare(
    State(st): State<AppState>,
    Json(body): Json<PerfPrepareBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || {
        ops::perf_prepare(
            shared.as_ref(),
            &body.uuid,
            &body.variant,
            &body.profile,
            "http",
        )
    })
    .await
}

async fn v1_perf_arm(State(st): State<AppState>, Json(body): Json<PerfEpochBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::perf_arm(shared.as_ref(), body.epoch, "http"))
        .await
}

async fn v1_perf_freeze(
    State(st): State<AppState>,
    Json(body): Json<PerfEpochBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::perf_freeze(shared.as_ref(), body.epoch, "http"))
        .await
}

async fn v1_perf_read(State(st): State<AppState>, Json(body): Json<PerfReadBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || {
        ops::perf_read(
            shared.as_ref(),
            body.epoch,
            body.offset,
            body.max_bytes,
            "http",
        )
    })
    .await
}

async fn v1_perf_release(
    State(st): State<AppState>,
    Json(body): Json<PerfEpochBody>,
) -> Json<ApiResp> {
    let shared = st.shared.clone();
    st.reply(move || ops::perf_release(shared.as_ref(), body.epoch, "http"))
        .await
}

async fn v1_get_serial(State(st): State<AppState>, Json(body): Json<SerialBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let since = body.since_unix_ms;
    let until = body.until_unix_ms;
    st.reply(move || ops::get_serial(shared.as_ref(), since, until, "http"))
        .await
}

async fn v1_run(State(st): State<AppState>, Json(body): Json<RunBody>) -> Json<ApiResp> {
    let shared = st.shared.clone();
    let cmd = body.cmd;
    st.reply(move || ops::run_cmd(shared.as_ref(), &cmd, "http"))
        .await
}

#[cfg(test)]
mod clear_requests {
    use super::*;

    #[test]
    fn clear_requires_a_nonblank_name_or_a_single_wildcard() {
        for invalid in [
            serde_json::json!(""),
            serde_json::json!(" "),
            serde_json::json!([]),
            serde_json::json!(["", " "]),
            serde_json::json!(["valid", " "]),
            serde_json::json!(["valid", "*"]),
            serde_json::json!(["*", "*"]),
            serde_json::json!(" * "),
        ] {
            let name = serde_json::from_value(invalid.clone()).unwrap();
            assert!(clear_xdp_names(name).is_err(), "accepted {invalid}");
        }
        for all in [serde_json::json!("*"), serde_json::json!(["*"])] {
            assert!(
                clear_xdp_names(serde_json::from_value(all).unwrap())
                    .unwrap()
                    .is_empty()
            );
        }
        let names = serde_json::from_value(serde_json::json!(["valid", "other"])).unwrap();
        assert_eq!(clear_xdp_names(names).unwrap(), ["valid", "other"]);
    }
}
