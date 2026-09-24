//! TR3000 probe: RAW stimulus + TC stamps + compile-time TOML config.
//!
//! Formal benches (Postgres):
//! - `./test all [--smoke] [--formal]` — all formal benches in order
//! - `./test drop-xdp / drop-hal [--smoke] [--formal] [--window-secs N] [--rounds N]` — drop throughput → `xdp_drop_*`
//! - `./test echo [--smoke] [--formal]` — XDP full-swap RTT → `xdp_echo_*`
//! - `./test wl-rtt / wl-loss [--smoke] [--formal]` — whitelist α grid
//! - `./test rate-rtt / rate-loss [--smoke] [--formal]` — rate-BL peak%×mit
//!
//! `./test suite` — embedded `config/test.toml` [[tests]].
//! `./test serve` — long-running HTTP daemon (tee child output to terminal + API).
//! No subcommand → print help (no default bench).

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
#[command(name = "probe", about = "TR3000 host probe (formal benches + suite)")]
struct Args {
    /// Short windows / fewer cells for smoke.
    #[arg(long, global = true)]
    smoke: bool,
    /// Write into schema `formal` (same db `postgres`) and TRUNCATE this bench's tables before run.
    #[arg(long, global = true)]
    formal: bool,
    /// UDP blast window in seconds (overrides smoke=40s / formal=40s default).
    #[arg(long, global = true)]
    window_secs: Option<u64>,
    /// Skip Postgres ingest (stdout only).
    #[arg(long, global = true)]
    no_pg: bool,
    /// Skip just-drop UDP blast perf phase (functional only).
    #[arg(long, global = true)]
    no_perf: bool,
    /// Drop bench rounds (overrides smoke=1 / formal=3 default).
    #[arg(long, global = true)]
    rounds: Option<usize>,
    /// Print legacy pretty-JSON report to stdout (suite only; default: human table).
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
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
    /// Alias of `rate-rtt`.
    Bench,
    /// Alias of `wl-loss`.
    BenchLoss,
    /// HTTP daemon: accept remote jobs, re-exec `./test <bench>`, tee logs.
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
    let quiet = match &args.command {
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
            | Command::Bench
            | Command::BenchLoss
            | Command::Serve { .. },
        ) => true,
    };
    let log_default = if quiet { "warn" } else { "info" };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_default)).init();

    if args.command.is_none() {
        let mut cmd = Args::command();
        cmd.print_help()?;
        println!();
        return Ok(());
    }

    if matches!(args.command, Some(Command::Serve { .. })) {
        let Command::Serve { http_listen } = args.command.unwrap() else {
            unreachable!();
        };
        return serve::run(http_listen).await;
    }

    let cfg = FileConfig::embedded()?;
    info!(
        "embedded config esp={} mac={} iface={}",
        cfg.target.esp_ip, cfg.target.esp_mac, cfg.probe.iface
    );

    if matches!(args.command, Some(Command::JustDrop)) {
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

    let drop_opts = DropOpts::new(
        args.smoke,
        args.formal,
        args.no_pg,
        args.window_secs,
        args.rounds,
    );
    let mut harness = open_harness(&cfg).await?;
    match args.command {
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
