//! Mac-side client: ensure flash agent, submit TR3000 executor jobs, stream logs.

mod config;

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::sleep;

#[derive(Parser)]
#[command(
    name = "fwctl",
    about = "Submit test jobs to TR3000 executor and stream captured stdout/stderr"
)]
struct Cli {
    /// Executor HTTP base URL, e.g. http://192.168.110.1:19081
    #[arg(long, env = "EXECUTOR_HTTP", default_value = "http://127.0.0.1:19081")]
    executor: String,
    /// Mac flash agent endpoint (default from embedded config/test.toml).
    #[arg(long, env = "XPOST_AGENT_ENDPOINT")]
    agent: Option<String>,
    /// Start `cargo xtask agent` if the agent port is not reachable.
    #[arg(long, default_value_t = true)]
    ensure_agent: bool,
    /// Bench name: all, drop-xdp, echo, …
    bench: String,
    #[arg(long)]
    smoke: bool,
    #[arg(long)]
    formal: bool,
}

#[derive(Debug, Serialize)]
struct SubmitBody {
    bench: String,
    smoke: bool,
    formal: bool,
}

#[derive(Debug, Deserialize)]
struct SubmitReply {
    job_id: u64,
    status: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct JobReply {
    job_id: u64,
    state: u32,
    terminal: bool,
    bench: String,
    message: String,
    log_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct LogReply {
    offset: usize,
    next_offset: usize,
    chunk: String,
    eof: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let agent_endpoint = cli
        .agent
        .unwrap_or_else(|| config::embedded_agent_endpoint());

    if cli.ensure_agent {
        ensure_agent(&agent_endpoint).await?;
    }

    let base = cli.executor.trim_end_matches('/');
    let submit: SubmitReply = ureq::post(&format!("{base}/api/jobs"))
        .set("Content-Type", "application/json")
        .send_json(SubmitBody {
            bench: cli.bench.clone(),
            smoke: cli.smoke,
            formal: cli.formal,
        })?
        .into_json()
        .context("parse submit response")?;

    eprintln!(
        "job {} accepted ({}) bench={}",
        submit.job_id, submit.status, cli.bench
    );

    let mut log_off = 0usize;
    loop {
        let job: JobReply = ureq::get(&format!("{base}/api/jobs/{}", submit.job_id))
            .call()?
            .into_json()
            .context("parse job status")?;

        while log_off < job.log_bytes {
            let log: LogReply = ureq::get(&format!(
                "{base}/api/jobs/{}/log?offset={log_off}",
                submit.job_id
            ))
            .call()?
            .into_json()
            .context("parse job log")?;
            if !log.chunk.is_empty() {
                tokio::io::stdout()
                    .write_all(log.chunk.as_bytes())
                    .await
                    .context("write log to terminal")?;
                tokio::io::stdout().flush().await.ok();
            }
            log_off = log.next_offset;
            if log.eof {
                break;
            }
        }

        if job.terminal {
            eprintln!(
                "\njob {} finished: state={} message={}",
                job.job_id, job.state, job.message
            );
            if job.state != 4 {
                bail!("job failed (state={})", job.state);
            }
            return Ok(());
        }
        sleep(Duration::from_millis(500)).await;
    }
}

async fn ensure_agent(endpoint: &str) -> Result<()> {
    if tcp_probe(endpoint).await.is_ok() {
        eprintln!("agent reachable at {endpoint}");
        return Ok(());
    }

    let listen = agent_listen_addr(endpoint);
    let xpost = xpost_root();
    eprintln!("starting xpost agent on {listen}…");
    let _child = Command::new("cargo")
        .args(["xtask", "agent", "--listen", &listen, "--no-monitor"])
        .current_dir(&xpost)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn cargo xtask agent in {}", xpost.display()))?;

    for _ in 0..180 {
        sleep(Duration::from_secs(1)).await;
        if tcp_probe(endpoint).await.is_ok() {
            eprintln!("agent ready at {endpoint}");
            return Ok(());
        }
    }
    bail!("agent at {endpoint} did not become ready within 180s");
}

fn agent_listen_addr(endpoint: &str) -> String {
    let host = endpoint.rsplit_once(':').map(|(h, _)| h).unwrap_or("0.0.0.0");
    match host {
        "127.0.0.1" | "localhost" => "127.0.0.1:9240".into(),
        _ => format!("0.0.0.0:{}", endpoint.rsplit_once(':').map(|(_, p)| p).unwrap_or("9240")),
    }
}

async fn tcp_probe(endpoint: &str) -> Result<()> {
    let mut stream = TcpStream::connect(endpoint).await?;
    stream.shutdown().await.ok();
    Ok(())
}

fn xpost_root() -> PathBuf {
    if let Ok(root) = std::env::var("XPOST_ROOT") {
        return PathBuf::from(root);
    }
    // fwctl lives at xpost/xpost-test/fwctl → parent workspace is ../..
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}
