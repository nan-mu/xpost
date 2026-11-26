//! TR3000 host harness binary **`xpost-test`**.
//!
//! Sole entry: bare `./xpost-test` (no env, no flags). Experiments and knobs
//! come from compile-time TOML. Database settings come from `/root/.db`.
//!
#![doc = include_str!("overview.md")]
#![deny(dead_code)]
#![deny(unused_imports)]

mod agent;
mod config;
mod db;
mod exam;
mod pipeline;

use anyhow::Result;
use config::FileConfig;
use log::info;

#[tokio::main]
async fn main() -> Result<()> {
    use exam::medium::{self, Volume};
    let cfg = FileConfig::embedded()?;
    match cfg.run[0].as_str() {
        "all" => return medium::run(&cfg, Volume::All).await,
        "all-plan" => return medium::preview(cfg.medium_stage, Volume::All),
        "all-preflight" => return medium::check(&cfg, Volume::All).await,
        "db-preflight" => return db::protocol::check_database().await,
        "medium" => return medium::run(&cfg, Volume::Medium).await,
        "medium-plan" => return medium::preview(cfg.medium_stage, Volume::Medium),
        "medium-preflight" => return medium::check(&cfg, Volume::Medium).await,
        _ => {}
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    info!(
        "embedded config v={} esp={} mac={} iface={} run={:?}",
        cfg.version, cfg.target.esp_ip, cfg.target.esp_mac, cfg.probe.iface, cfg.run
    );

    let mut failed = Vec::new();
    for name in &cfg.run {
        info!("run experiment {name}");
        if let Err(err) = dispatch(&cfg, name).await {
            eprintln!("experiment {name} failed: {err:#}");
            failed.push(format!("{name}: {err:#}"));
        }
    }
    if !failed.is_empty() {
        anyhow::bail!("{}", failed.join("\n"));
    }
    Ok(())
}

async fn dispatch(cfg: &FileConfig, name: &str) -> Result<()> {
    match name {
        "exec_bench" => exam::run_exec_bench(cfg).await,
        "dev" => {
            let mut harness = pipeline::open_harness(cfg).await?;
            exam::run_dev(cfg, &mut harness).await
        }
        other => anyhow::bail!("internal: unknown run entry {other:?}"),
    }
}
