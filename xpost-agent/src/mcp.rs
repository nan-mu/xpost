//! MCP tools over Streamable HTTP (`/mcp`).

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};

use crate::ops;
use crate::state::SharedState;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct FlashArgs {
    profile_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ProgramArgs {
    elf: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RequestArgs {
    cmd: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ProbeArgs {
    esp: String,
    #[serde(default = "default_probe_port")]
    port: u16,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ProbeBurstArgs {
    esp: String,
    #[serde(default = "default_probe_port")]
    port: u16,
    count: u32,
}

fn default_probe_port() -> u16 {
    8080
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

    #[tool(description = "Flash a firmware profile (build + USB program) from profiles.toml")]
    async fn flash_profile(
        &self,
        Parameters(args): Parameters<FlashArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let id = args.profile_id;
        self.run_op(move || ops::flash_profile(shared.as_ref(), &id, "mcp"))
            .await
    }

    #[tool(description = "Install an XDP ELF into the cold table by path")]
    async fn install_xdp(
        &self,
        Parameters(args): Parameters<ProgramArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let elf = args.elf;
        self.run_op(move || ops::program(shared.as_ref(), &elf, "mcp"))
            .await
    }

    #[tool(description = "Erase the entire XDP cold table")]
    async fn clear_xdp(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::clear_xdp(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "List XDP ELF entries in the cold table")]
    async fn list_xdp(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::list_xdp(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "Read firmware counters via App-mode serial")]
    async fn counters(&self) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        self.run_op(move || ops::counters(shared.as_ref(), "mcp"))
            .await
    }

    #[tool(description = "Send one App-mode serial command (e.g. ping, rxcnt 17, drop on)")]
    async fn request(
        &self,
        Parameters(args): Parameters<RequestArgs>,
    ) -> Result<CallToolResult, McpError> {
        let shared = self.shared.clone();
        let cmd = args.cmd;
        self.run_op(move || ops::request(shared.as_ref(), &cmd, "mcp"))
            .await
    }

    #[tool(description = "UDP STIM probe; returns ack or no-ack")]
    async fn probe(
        &self,
        Parameters(args): Parameters<ProbeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let esp = args.esp;
        let port = args.port;
        self.run_op(move || ops::probe(&esp, port, "mcp")).await
    }

    #[tool(description = "Fire-and-forget N UDP STIMs; returns sent=N")]
    async fn probe_burst(
        &self,
        Parameters(args): Parameters<ProbeBurstArgs>,
    ) -> Result<CallToolResult, McpError> {
        let esp = args.esp;
        let port = args.port;
        let count = args.count;
        self.run_op(move || ops::probe_burst_op(&esp, port, count, "mcp"))
            .await
    }

    #[tool(description = "Agent status (always idle when accepting requests)")]
    async fn get_status(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(ops::get_status())]))
    }
}

#[tool_handler]
impl ServerHandler for XpostMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder().enable_tools().build(),
        )
        .with_instructions(
            "xpost-agent: flash profiles, install/list/clear XDP ELF, serial request, UDP probe.",
        )
    }
}
