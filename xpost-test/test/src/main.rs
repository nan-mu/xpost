//! TR3000 host harness binary **`xpost-test`**.
//!
//! Sole entry: bare `./xpost-test` (no env, no flags). Experiments and knobs
//! come from compile-time `config/default.toml` + `just_tx.toml` /
//! `just_drop.toml` — see [`config`]. Workflow overview: [`overview`].

mod agent;
mod bench;
mod config;
mod harness;
mod overview;
mod raw_tx;

use anyhow::{Context, Result};
use config::{FileConfig, format_mac, install_postgres};
use harness::Harness;
use log::info;
use raw_tx::{RawTx, iface_mac};

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
            bench::run_just_drop_pps(
                cfg,
                bench::JustDropPpsOpts {
                    formal: cfg.just_drop.formal,
                },
            )
            .await
        }
        "just_tx" => {
            let mut harness = open_harness(cfg).await?;
            bench::run_just_tx(
                cfg,
                &mut harness,
                bench::JustTxOpts {
                    is_dev: false,
                    formal: cfg.just_tx.formal,
                },
            )
            .await
        }
        "dev" => {
            let mut harness = open_harness(cfg).await?;
            bench::run_just_tx(
                cfg,
                &mut harness,
                bench::JustTxOpts {
                    is_dev: true,
                    formal: false,
                },
            )
            .await
        }
        "formal-clear" => {
            let pg_opts = bench::pg_dsn::PgOpts { formal: true };
            bench::formal::formal_clear(pg_opts).await
        }
        other => anyhow::bail!("internal: unknown run entry {other:?}"),
    }
}

async fn open_harness(cfg: &FileConfig) -> Result<Harness> {
    let esp_mac = cfg.esp_mac_bytes()?;
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => iface_mac(&cfg.probe.iface)?,
    };
    info!(
        "RAW src_mac={} dst_mac={} iface={}",
        format_mac(&src_mac),
        format_mac(&esp_mac),
        cfg.probe.iface
    );

    if cfg.target.stim_sport == cfg.target.reply_port {
        anyhow::bail!(
            "dut.stim_sport ({}) must differ from reply_port (fw_hash bounce TC)",
            cfg.target.stim_sport
        );
    }
    let raw = RawTx::open(
        &cfg.probe.iface,
        src_mac,
        esp_mac,
        cfg.target.esp_ip,
        cfg.target.echo_port,
        cfg.target.stim_sport,
    )?;

    Harness::start(
        cfg.probe.iface.clone(),
        cfg.target.esp_ip,
        Some(cfg.target.vip_ip),
        cfg.target.echo_port,
        cfg.target.reply_port,
        cfg.target.ctrl_port,
        cfg.probe.pair_timeout_ms,
        raw,
    )
    .await
    .context("start harness")
}
