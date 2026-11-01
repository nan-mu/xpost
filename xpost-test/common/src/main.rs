//! TR3000 host harness binary **`xpost-test`**.
//!
//! Sole entry: bare `./xpost-test` (no env, no flags). Experiments and knobs
//! come from compile-time `config/default.toml` + `just_tx.toml` /
//! `just_drop.toml` — see [`config`].
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
        "just_drop" => {
            exam::run_just_drop_pps(
                cfg,
                exam::JustDropPpsOpts {
                    formal: cfg.just_drop.formal,
                    rates: None,
                    lookup: false,
                    skip_trace: false,
                },
            )
            .await
        }
        "firewall" => {
            exam::run_firewall(
                cfg,
                exam::FirewallOpts {
                    formal: cfg.firewall.formal,
                    rates: None,
                    lookup: false,
                    skip_trace: false,
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
                    rates: None,
                    lookup_pair: false,
                    skip_trace: false,
                },
            )
            .await
        }
        "lookup" => {
            let mut harness = pipeline::open_harness(cfg).await?;
            exam::run_just_tx(
                cfg,
                &mut harness,
                exam::JustTxOpts {
                    is_dev: false,
                    formal: true,
                    rates: Some(&[1400.0]),
                    lookup_pair: true,
                    skip_trace: true,
                },
            )
            .await?;
            exam::run_just_drop_pps(
                cfg,
                exam::JustDropPpsOpts {
                    formal: true,
                    rates: Some(exam::LOOKUP_RATES),
                    lookup: true,
                    skip_trace: true,
                },
            )
            .await?;
            exam::run_firewall(
                cfg,
                exam::FirewallOpts {
                    formal: true,
                    rates: Some(exam::LOOKUP_RATES),
                    lookup: true,
                    skip_trace: true,
                },
            )
            .await
        }
        "case" => exam::run_case(cfg).await,
        "exec_bench" => {
            exam::run_exec_bench(
                cfg,
                exam::ExecBenchOpts {
                    formal: cfg.exec_bench.formal,
                },
            )
            .await
        }
        "dev" => {
            let mut harness = pipeline::open_harness(cfg).await?;
            exam::run_dev(cfg, &mut harness).await
        }
        "formal-clear" => {
            let pg_opts = db::PgOpts { formal: true };
            db::formal_clear(pg_opts).await
        }
        other => anyhow::bail!("internal: unknown run entry {other:?}"),
    }
}
