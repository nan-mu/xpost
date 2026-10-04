//! TR3000 host harness binary **`xpost-test`**.
//!
//! Sole entry: bare `./xpost-test` (no env, no flags). Experiments and knobs
//! come from compile-time `config/default.toml` + `just_tx.toml` /
//! `just_drop.toml` — see [`config`]. Workflow overview: [`overview`].

mod agent;
mod config;
mod db;
mod exam;
mod overview;
mod pipeline;

use anyhow::Result;
use config::{FileConfig, install_postgres};
use log::info;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = FileConfig::embedded()?;
    install_postgres(cfg.postgres.clone());

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    info!(
        "embedded config v={} esp={} mac={} iface={} run={:?}",
        cfg.version, cfg.target.esp_ip, cfg.target.esp_mac, cfg.probe.iface, cfg.run
    );

    for name in &cfg.run {
        info!("run experiment {name}");
        dispatch(&cfg, name).await?;
    }
    Ok(())
}

async fn dispatch(cfg: &FileConfig, name: &str) -> Result<()> {
    match name {
        "just_drop" => {
            exam::run_just_drop_pps(
                cfg,
                exam::JustDropPpsOpts {
                    formal: cfg.just_drop.formal,
                },
            )
            .await
        }
        "firewall" => {
            exam::run_firewall(
                cfg,
                exam::FirewallOpts {
                    formal: cfg.firewall.formal,
                },
            )
            .await
        }
        "just_tx" => {
            let mut harness = pipeline::open_harness(cfg).await?;
            exam::run_just_tx(
                cfg,
                &mut harness,
                exam::JustTxOpts {
                    is_dev: false,
                    formal: cfg.just_tx.formal,
                },
            )
            .await
        }
        "dev" => {
            let mut harness = pipeline::open_harness(cfg).await?;
            exam::run_just_tx(
                cfg,
                &mut harness,
                exam::JustTxOpts {
                    is_dev: true,
                    formal: false,
                },
            )
            .await
        }
        "formal-clear" => {
            let pg_opts = db::PgOpts { formal: true };
            db::formal_clear(pg_opts).await
        }
        other => anyhow::bail!("internal: unknown run entry {other:?}"),
    }
}
