mod child;
mod prep;
mod profiles;
mod rpc;
mod serial;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tokio::signal;

use serial::SerialSupervisor;

#[derive(Parser)]
#[command(name = "xpost-agent", about = "Mac-side ESP flash agent (RPC)")]
struct Cli {
    /// Listen address (default all interfaces on LAN; TR3000 uses Mac LAN IP).
    #[arg(long, default_value = "0.0.0.0:9240")]
    listen: String,
    /// xpost workspace root (profiles.toml + profile bins).
    #[arg(long)]
    root: Option<PathBuf>,
    /// Skip build/doctor/check before listen (dev only).
    #[arg(long)]
    skip_prep: bool,
    /// Do not keep `espflash monitor` on this tty (headless / CI).
    #[arg(long)]
    no_monitor: bool,
}

fn xpost_root(cli: &Cli) -> PathBuf {
    cli.root.clone().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("agent parent")
            .to_path_buf()
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = xpost_root(&cli);
    let profiles = profiles::load_profiles(&root).context("load profiles")?;
    if profiles.is_empty() {
        anyhow::bail!("no profiles in {}", root.join("profiles.toml").display());
    }
    // Sync prep: keeps Ctrl+C on the shell process group (not blocked inside tokio).
    if !cli.skip_prep {
        eprintln!(
            "xpost-agent: building profiles (doctor/check for firmware, then release build)…"
        );
        prep::prepare_profiles(&root, &profiles).context("profile prep")?;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    rt.block_on(run_agent(cli, profiles))
}

async fn run_agent(cli: Cli, profiles: Vec<profiles::Profile>) -> Result<()> {
    let chip = profiles
        .first()
        .map(|p| p.chip.as_str())
        .unwrap_or("esp32c3");
    let serial = Arc::new(SerialSupervisor::new(chip, !cli.no_monitor));
    let state = rpc::AgentState::new(profiles, serial.clone());
    let addr: SocketAddr = cli.listen.parse().context("parse --listen")?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    eprintln!(
        "xpost-agent listening on {addr} (profiles from {}; monitor={}; deploy on FlashProfile)",
        xpost_root(&cli).display(),
        if serial.monitor_enabled() {
            "on"
        } else {
            "off"
        }
    );
    eprintln!("xpost-agent: Ctrl+C stops agent and kills espflash children");

    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => {
                eprintln!();
                eprintln!("xpost-agent: shutting down …");
                serial.shutdown().await;
                return Ok(());
            }
            accept = listener.accept() => {
                let (stream, peer) = accept.context("accept")?;
                eprintln!("client {peer}");
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = state.handle_connection(stream).await {
                        eprintln!("rpc session ended: {e:#}");
                    }
                });
            }
        }
    }
}
