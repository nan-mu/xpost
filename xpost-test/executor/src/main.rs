mod agent_client;
mod config;
mod http;
mod job_log;
mod jobs;
mod rpc;
mod run;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;

use agent_client::AgentClient;
use config::ExecutorConfig;
use http::HttpState;
use jobs::Jobs;
use rpc::RpcState;

#[derive(Parser)]
#[command(name = "executor", about = "TR3000 probe job executor (RPC + HTTP)")]
struct Cli {
    /// Override RPC listen address (default from config `[executor].listen`).
    #[arg(long)]
    listen: Option<String>,
    /// Override HTTP listen address (default from config `[executor].http_listen`).
    #[arg(long, alias = "http")]
    http_listen: Option<String>,
    /// Mac flash agent endpoint (default from config `[agent].endpoint`).
    #[arg(long)]
    agent_endpoint: Option<String>,
    /// Path to probe binary (default from config `[executor].probe_bin`).
    #[arg(long, alias = "test-bin")]
    probe_bin: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let cfg = ExecutorConfig::embedded()?;

    let listen = cli
        .listen
        .unwrap_or_else(|| cfg.executor.listen.clone());
    let http_listen = cli
        .http_listen
        .unwrap_or_else(|| cfg.executor.http_listen.clone());
    let agent_endpoint = cli
        .agent_endpoint
        .unwrap_or_else(|| cfg.agent_endpoint());
    let probe_bin = cli
        .probe_bin
        .unwrap_or_else(|| cfg.executor.probe_bin.clone());

    let exec_cfg = config::ExecutorConfig {
        agent: config::AgentSection {
            endpoint: agent_endpoint.clone(),
        },
        executor: config::ExecutorSection {
            listen: listen.clone(),
            http_listen: http_listen.clone(),
            probe_bin,
        },
    };

    let state = RpcState {
        jobs: Arc::new(Jobs::new()),
        agent: AgentClient::new(agent_endpoint.clone()),
        cfg: exec_cfg,
    };

    let http_state = HttpState { rpc: state.clone() };
    let http_bind = http_listen.clone();
    tokio::spawn(async move {
        if let Err(e) = http::serve(&http_bind, http_state).await {
            eprintln!("HTTP server error: {e:#}");
        }
    });

    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    eprintln!(
        "executor RPC on {listen}  HTTP on {http_listen}  (agent={agent_endpoint}, probe={})",
        state.cfg.executor.probe_bin
    );

    loop {
        let (stream, peer) = listener.accept().await.context("accept")?;
        eprintln!("rpc client {peer}");
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = state.handle_connection(stream).await {
                eprintln!("rpc session ended: {e:#}");
            }
        });
    }
}
