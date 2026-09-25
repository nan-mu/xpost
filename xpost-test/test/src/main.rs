//! TR3000 host harness: RAW stimulus + compile-time TOML config.
//!
//! Binary: `xpost-test`. Bare `./xpost-test` runs `[default_bench]` from embedded
//! `config/test.toml` (default `just-drop-pps`).
//!
//! Formal benches (Postgres):
//! - `./xpost-test all [--smoke] [--formal]` — all formal benches in order
//! - `./xpost-test just-drop-pps` — flash HAL/XDP profiles, sample drop → `experiment_*`
//! - `./xpost-test drop-xdp / drop-hal / echo / …` — legacy benches
//!
//! `./xpost-test suite` — embedded `config/test.toml` [[tests]].
//! `./xpost-test serve` — long-running HTTP daemon.
//! `./xpost-test --help-benches` — print CLI help without running default.

mod agent;
mod bench;
mod config;
mod generated;
mod harness;
mod raw_tx;
mod serve;
mod tests;

use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use bench::drop_common::DropOpts;
use clap::{CommandFactory, Parser, Subcommand};
use config::{FileConfig, format_mac};
use harness::Harness;
use log::{debug, info, warn};
use raw_tx::{RawTx, iface_mac};
use test_report::RunReport;
use tests::lookup;

#[derive(Debug, Parser)]
#[command(name = "xpost-test", about = "TR3000 host harness (formal benches + suite)")]
struct Args {
    /// DEBUG only: short windows / fewer pairs. Not used in formal performance runs.
    #[arg(long, global = true, hide = true)]
    smoke: bool,
    /// Write into schema `formal` (same db `postgres`) and empty this bench's tables before run.
    #[arg(long, global = true)]
    formal: bool,
    /// UDP blast window in seconds (overrides smoke / embedded default).
    #[arg(long, global = true)]
    window_secs: Option<u64>,
    /// Skip Postgres ingest (stdout only).
    #[arg(long, global = true)]
    no_pg: bool,
    /// Skip just-drop UDP blast perf phase (functional only).
    #[arg(long, global = true)]
    no_perf: bool,
    /// Pair / round count (overrides smoke=1 / embedded default).
    #[arg(long, global = true)]
    rounds: Option<usize>,
    /// Print legacy pretty-JSON report to stdout (suite only; default: human table).
    #[arg(long)]
    json: bool,
    /// Print CLI help without running `[default_bench]`.
    #[arg(long)]
    help_benches: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Run every formal bench in order (inherits `--smoke` / `--formal`).
    All,
    /// Run embedded `config/test.toml` [[tests]] (former default).
    Suite,
    /// 丢包实验 XDP path via profile boot → `xdp_drop_*` (`path=xdp_drop`).
    #[command(name = "drop-xdp")]
    DropXdp,
    /// Pure HAL RX drop counter → `xdp_drop_*` (`path=hal_drop`).
    #[command(name = "drop-hal")]
    DropHal,
    /// XDP full-swap bounce RTT → `xdp_echo_*`.
    Echo,
    /// Whitelist userspace RTT under α×hping (incl α=0) → `xdp_ddos_*`.
    WlRtt,
    /// Whitelist byte-stream loss under α×hping (incl α=0) → `xdp_loss_*`.
    WlLoss,
    /// Rate-blacklist online U-echo RTT (peak% × mit) → `xdp_rate_rtt_*`.
    RateRtt,
    /// Rate-blacklist collect→dump loss (peak% × mit) → `xdp_rate_loss_*`.
    RateLoss,
    /// just_drop ACK path: clear vs drop XDP → `xdp_ack_*`.
    #[command(name = "just-drop")]
    JustDrop,
    /// just_drop / just_drop_hal PPS → `experiment_*` (agent flashes each arm).
    #[command(name = "just-drop-pps")]
    JustDropPps {
        /// Restrict to one arm. Omit → embedded `[just_drop_pps].arms`.
        #[arg(long, value_parser = ["xdp", "hal"])]
        path: Option<String>,
        /// Counter sample period while blasting (ms). `0` → embedded default.
        #[arg(long, default_value_t = 0)]
        sample_interval_ms: u64,
    },
    /// just_tx / just_tx_hal PPS → `experiment_*` via `rxcnt 17` (agent flashes each arm).
    #[command(name = "just-tx-pps")]
    JustTxPps {
        /// Restrict to one arm. Omit → embedded `[just_tx_pps].arms`.
        #[arg(long, value_parser = ["xdp", "hal"])]
        path: Option<String>,
        /// Counter sample period while blasting (ms). `0` → embedded default.
        #[arg(long, default_value_t = 0)]
        sample_interval_ms: u64,
    },
    /// Alias of `rate-rtt`.
    Bench,
    /// Alias of `wl-loss`.
    BenchLoss,
    /// HTTP daemon: accept remote jobs, re-exec `./xpost-test <bench>`, tee logs.
    Serve {
        /// Override `[serve].http_listen` from embedded config.
        #[arg(long)]
        http_listen: Option<String>,
    },
}

fn wall_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = FileConfig::embedded()?;

    if args.help_benches {
        let mut cmd = Args::command();
        cmd.print_help()?;
        println!();
        return Ok(());
    }

    let command = if args.command.is_none() {
        match cfg.default_bench.as_str() {
            "just-drop-pps" => Some(Command::JustDropPps {
                path: None,
                sample_interval_ms: 0,
            }),
            "suite" => Some(Command::Suite),
            "all" => Some(Command::All),
            other => {
                anyhow::bail!(
                    "unknown [default_bench]={other:?}; edit config/test.toml or pass a subcommand"
                );
            }
        }
    } else {
        args.command.clone()
    };

    let quiet = match &command {
        None | Some(Command::Suite) => false,
        Some(
            Command::All
            | Command::DropXdp
            | Command::DropHal
            | Command::Echo
            | Command::WlRtt
            | Command::WlLoss
            | Command::RateRtt
            | Command::RateLoss
            | Command::JustDrop
            | Command::JustDropPps { .. }
            | Command::JustTxPps { .. }
            | Command::Bench
            | Command::BenchLoss
            | Command::Serve { .. },
        ) => true,
    };
    let log_default = if quiet { "warn" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_default)).init();

    if matches!(command, Some(Command::Serve { .. })) {
        let Some(Command::Serve { http_listen }) = command else {
            unreachable!();
        };
        return serve::run(http_listen).await;
    }

    info!(
        "embedded config esp={} mac={} iface={} default_bench={}",
        cfg.target.esp_ip, cfg.target.esp_mac, cfg.probe.iface, cfg.default_bench
    );

    if matches!(command, Some(Command::JustDrop)) {
        return bench::run_just_drop(
            &cfg,
            bench::JustDropOpts {
                formal: args.formal,
                no_pg: args.no_pg,
                no_perf: args.no_perf,
                smoke: args.smoke,
                window_secs: args.window_secs,
            },
        )
        .await;
    }
    if let Some(Command::JustDropPps {
        path,
        sample_interval_ms,
    }) = &command
    {
        let paths = match path {
            Some(p) => vec![bench::PpsPath::parse(p)?],
            None => Vec::new(),
        };
        let sample_ms = if *sample_interval_ms == 0 {
            cfg.just_drop_pps.sample_interval_ms
        } else {
            *sample_interval_ms
        };
        return bench::run_just_drop_pps(
            &cfg,
            bench::JustDropPpsOpts {
                paths,
                smoke: args.smoke,
                formal: args.formal || cfg.just_drop_pps.formal,
                no_pg: args.no_pg,
                window_secs: args.window_secs,
                sample_interval_ms: sample_ms,
                rounds: args.rounds,
            },
        )
        .await;
    }
    if let Some(Command::JustTxPps {
        path,
        sample_interval_ms,
    }) = &command
    {
        let paths = match path {
            Some(p) => vec![bench::PpsPath::parse(p)?],
            None => Vec::new(),
        };
        let sample_ms = if *sample_interval_ms == 0 {
            cfg.just_tx_pps.sample_interval_ms
        } else {
            *sample_interval_ms
        };
        return bench::run_just_tx_pps(
            &cfg,
            bench::JustTxPpsOpts {
                paths,
                smoke: args.smoke,
                formal: args.formal || cfg.just_tx_pps.formal,
                no_pg: args.no_pg,
                window_secs: args.window_secs,
                sample_interval_ms: sample_ms,
                rounds: args.rounds,
            },
        )
        .await;
    }

    let drop_opts = DropOpts::new(
        args.smoke,
        args.formal,
        args.no_pg,
        args.window_secs,
        args.rounds,
    );
    let mut harness = open_harness(&cfg).await?;
    match command {
        Some(Command::Serve { .. }) => unreachable!(),
        Some(Command::All) => {
            return bench::run_all_benches(&mut harness, &cfg, drop_opts).await;
        }
        Some(Command::DropXdp) => {
            ensure_bench_profile(&harness, &cfg, "drop-xdp").await?;
            return bench::run_drop_xdp(&mut harness, drop_opts).await;
        }
        Some(Command::DropHal) => {
            ensure_bench_profile(&harness, &cfg, "drop-hal").await?;
            return bench::run_drop_hal(&mut harness, drop_opts).await;
        }
        Some(Command::Echo) => {
            ensure_bench_profile(&harness, &cfg, "echo").await?;
            return bench::run_echo(&mut harness, args.smoke, args.formal).await;
        }
        Some(Command::WlRtt) => {
            ensure_bench_profile(&harness, &cfg, "wl-rtt").await?;
            return bench::run_wl_rtt(&mut harness, args.smoke, args.formal).await;
        }
        Some(Command::WlLoss) | Some(Command::BenchLoss) => {
            ensure_bench_profile(&harness, &cfg, "wl-loss").await?;
            return bench::run_wl_loss(&mut harness, args.smoke, args.formal).await;
        }
        Some(Command::RateRtt) | Some(Command::Bench) => {
            ensure_bench_profile(&harness, &cfg, "rate-rtt").await?;
            return bench::run_rate_rtt(&mut harness, args.smoke, args.formal).await;
        }
        Some(Command::RateLoss) => {
            ensure_bench_profile(&harness, &cfg, "rate-loss").await?;
            return bench::run_rate_loss(&mut harness, args.smoke, args.formal).await;
        }
        Some(Command::JustDrop) => unreachable!("handled before harness open"),
        Some(Command::JustDropPps { .. }) => unreachable!("handled before harness open"),
        Some(Command::JustTxPps { .. }) => unreachable!("handled before harness open"),
        Some(Command::Suite) => {}
        None => unreachable!(),
    }
    let report = run_all(&cfg, &mut harness).await?;

    if args.json {
        let json = serde_json::to_string_pretty(&report)?;
        if !cfg.probe.report_path.is_empty() {
            fs::write(&cfg.probe.report_path, &json)
                .with_context(|| format!("write report {}", cfg.probe.report_path))?;
            info!("report written to {}", cfg.probe.report_path);
        }
        println!("{json}");
    } else {
        print!("{}", report.format_table());
    }

    if report.ok {
        info!("all tests passed ({})", wall_unix_ms());
        Ok(())
    } else {
        warn!("one or more tests failed");
        std::process::exit(1);
    }
}

async fn ensure_bench_profile(harness: &Harness, cfg: &FileConfig, bench: &str) -> Result<()> {
    let mut flashed = None;
    bench::flash::ensure_profile(harness, cfg, bench, &mut flashed).await
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
            "target.stim_sport ({}) must differ from reply_port (fw_hash bounce TC)",
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
        cfg.target.vip_ip,
        cfg.target.echo_port,
        cfg.target.reply_port,
        cfg.target.ctrl_port,
        cfg.probe.pair_timeout_ms,
        raw,
    )
    .await
}

async fn run_all(cfg: &FileConfig, harness: &mut Harness) -> Result<RunReport> {
    let mut test_reports = Vec::new();
    for entry in &cfg.tests {
        if !entry.enabled {
            debug!("skip disabled test {}", entry.name);
            continue;
        }
        let test = lookup(&entry.name)?;
        info!("running test {}", entry.name);
        let report = test.run(harness, &entry.cases).await?;
        test_reports.push(report);
    }
    Ok(RunReport::from_tests(
        cfg.probe.iface.clone(),
        cfg.target.esp_ip.to_string(),
        test_reports,
    ))
}
