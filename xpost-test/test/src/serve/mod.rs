mod http;
mod job_log;
mod jobs;
mod run;

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::signal;

use crate::config::FileConfig;

use self::http::HttpState;
use self::jobs::Jobs;

pub async fn run(http_listen: Option<String>) -> Result<()> {
    let cfg = FileConfig::embedded()?;
    let listen = http_listen.unwrap_or_else(|| cfg.serve.http_listen.clone());
    let agent_endpoint = cfg.agent.resolved_endpoint();

    let state = HttpState {
        jobs: Arc::new(Jobs::new()),
        agent_endpoint,
    };

    let bind = listen.clone();
    tokio::spawn(async move {
        if let Err(e) = http::serve(&bind, state).await {
            eprintln!("serve HTTP error: {e:#}");
        }
    });

    eprintln!("test serve ready (Ctrl+C to stop)");
    signal::ctrl_c().await.context("ctrl_c")?;
    eprintln!("test serve stopping");
    Ok(())
}
