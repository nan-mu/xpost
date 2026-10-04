//! MCP tools over Streamable HTTP (`/mcp`).

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};

use crate::ops::{self, MapKv};
use crate::state::SharedState;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FlashByNameArgs {
    name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ClearXdpArgs {
    /// `"*"` clears all; otherwise a program name (or comma-separated names).
    name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct MapNameArgs {
    name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct UpdateMapArgs {
    name: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    entries: Option<Vec<Kv>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct Kv {
    key: String,
    value: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct SerialArgs {
    since_unix_ms: u64,
    until_unix_ms: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RunArgs {
    cmd: String,
}

#[derive(Clone)]
pub struct XpostMcp {
    shared: SharedState,
    #[allow(dead_code)] // filled by #[tool_router]; used via #[tool_handler]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl XpostMcp {
    pub fn new(shared: SharedState) -> Self {
        Self {
            shared,
            tool_router: Self::tool_router(),
        }
    }

    async fn run_op<F>(&self, f: F) -> Result<CallToolResult, McpError>
    where
        F: FnOnce() -> anyhow::Result<String> + Send + 'static,
    {
        match tokio::task::spawn_blocking(f).await {
            Ok(Ok(msg)) => Ok(CallToolResult::success(vec![ContentBlock::text(msg)])),
            Ok(Err(e)) => Err(McpError::internal_error(format!("{e:#}"), None)),
            Err(e) => Err(McpError::internal_error(format!("join: {e}"), None)),
        }
    }

    #[tool(description = "Build + flash named firmware profile, then co-flash fw.xdp[]")]
    async fn flash_fw(
        &self,
        Parameters(args): Parameters<FlashByNameArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let name = args.name;
        self.run_op(move || ops::flash_fw(shared.as_ref(), &name, "mcp"))
            .await
    }

    #[tool(description = "Build + write named XDP profile into the cold table")]
    async fn flash_xdp(
        &self,
        Parameters(args): Parameters<FlashByNameArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let name = args.name;
        self.run_op(move || ops::flash_xdp(shared.as_ref(), &name, "mcp"))
            .await
    }

    #[tool(description = "Clear XDP cold-table image(s); name=\"*\" clears all")]
    async fn clear_xdp(
        &self,
        Parameters(args): Parameters<ClearXdpArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let name = args.name.trim().to_string();
        if name.is_empty() {
            return Err(McpError::invalid_params("missing name", None));
        }
        let names = if name == "*" {
            Vec::new()
        } else {
            name.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        self.run_op(move || ops::clear_xdp(shared.as_ref(), names, "mcp"))
            .await
    }

    #[tool(description = "List XDP images in the cold table")]
    async fn get_xdp(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::get_xdp(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "App EnableXdp")]
    async fn enable_xdp(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::enable_xdp(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "App DisableXdp")]
    async fn disable_xdp(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::disable_xdp(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "App ClearMap")]
    async fn clear_map(
        &self,
        Parameters(args): Parameters<MapNameArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let name = args.name;
        self.run_op(move || ops::clear_map(shared.as_ref(), &name, "mcp"))
            .await
    }

    #[tool(description = "App GetMap → OK get map <n> b64=…")]
    async fn get_map(
        &self,
        Parameters(args): Parameters<MapNameArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let name = args.name;
        self.run_op(move || ops::get_map(shared.as_ref(), &name, "mcp"))
            .await
    }

    #[tool(description = "App UpdateMap (single key/value or entries[])")]
    async fn update_map(
        &self,
        Parameters(args): Parameters<UpdateMapArgs>,
    ) -> Result<CallToolResult, McpError> {
        let entries = match (&args.key, &args.value, &args.entries) {
            (Some(k), Some(v), _) => vec![MapKv {
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
                return Err(McpError::invalid_params(
                    "need key+value or non-empty entries",
                    None,
                ));
            }
        };
        let shared = self.shared.clone();
        let name = args.name;
        self.run_op(move || ops::update_map(shared.as_ref(), &name, &entries, "mcp"))
            .await
    }

    #[tool(description = "App ClearPerf")]
    async fn clear_perf(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::clear_perf(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "App GetPerf → OK get perf <n> b64=…")]
    async fn get_perf(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::get_perf(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "Serial transcript lines in a host unix-ms window")]
    async fn get_serial(
        &self,
        Parameters(args): Parameters<SerialArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let since = args.since_unix_ms;
        let until = args.until_unix_ms;
        self.run_op(move || ops::get_serial(shared.as_ref(), since, until, "mcp"))
            .await
    }

    #[tool(description = "Debug: one App command line (not for get map/perf framing)")]
    async fn run(
        &self,
        Parameters(args): Parameters<RunArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let cmd = args.cmd;
        self.run_op(move || ops::run_cmd(shared.as_ref(), &cmd, "mcp"))
            .await
    }

    #[tool(description = "Agent status (always idle when accepting requests)")]
    async fn get_status(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(
            ops::get_status(),
        )]))
    }

    #[tool(description = "DUT UART settle: ready (ping ok) or booting")]
    async fn dut_status(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::dut_status(shared.as_ref())).await
    }
}

#[tool_handler]
impl ServerHandler for XpostMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "xpost-agent: flash fw/xdp, clear/get xdp, enable/disable xdp, map/perf/serial, run.",
            )
    }
}
