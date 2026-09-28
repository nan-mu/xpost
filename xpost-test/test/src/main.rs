//! TR3000 host harness: RAW stimulus + compile-time TOML config.
//!
//! Binary: `xpost-test`. Formal path on TR3000 is always bare:
//!   ./xpost-test
//! which runs `default.toml` `run = [...]` (no env, no flags).
//! `run` names whole experiments (`just_tx`, `just_drop`); steps are fixed in code.
//! Experiment knobs: `config/just_tx.toml`, `config/just_drop.toml`.
//! Shared DUT / agent / postgres: `config/default.toml`. See `config/README.md`.

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
use config::{FileConfig, format_mac, install_postgres};
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
    #[arg(long, global = true, hide = true)]
    formal: bool,
    /// UDP blast window in seconds (overrides smoke / embedded default).
    #[arg(long, global = true, hide = true)]
    window_secs: Option<u64>,
    /// Skip Postgres ingest (stdout only).
    #[arg(long, global = true, hide = true)]
    no_pg: bool,
    /// Skip just-drop UDP blast perf phase (functional only).
    #[arg(long, global = true, hide = true)]
    no_perf: bool,
    /// Pair / round count (overrides smoke=1 / embedded default).
    #[arg(long, global = true, hide = true)]
    rounds: Option<usize>,
    /// Print legacy pretty-JSON report to stdout (suite only; default: human table).
    #[arg(long)]
    json: bool,
    /// Print CLI help without running `default.toml` `run`.
    #[arg(long)]
    help_benches: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Run every formal bench in order (inherits `--smoke` / `--formal`).
    All,
    /// Run fixed functional suite (`test0`; not selected via `run`).
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
    /// Formal `just_drop` experiment. Same as `run = ["just_drop"]`.
    #[command(name = "just_drop")]
    JustDropExp {
        /// Restrict to one arm. Omit → embedded `just_drop.toml` arms.
        #[arg(long, value_parser = ["xdp", "hal"])]
        path: Option<String>,
        /// Counter sample period while blasting (ms). `0` → embedded default.
        #[arg(long, default_value_t = 0)]
        sample_interval_ms: u64,
    },
    /// Formal `just_tx` experiment. Same as `run = ["just_tx"]`.
    #[command(name = "just_tx")]
    JustTxExp {
        /// Restrict to one arm. Omit → embedded `just_tx.toml` arms.
        #[arg(long, value_parser = ["xdp", "hal"])]
        path: Option<String>,
    },
    /// Hardcoded XDP TX debug: 800/1600/2800/4000 × 1 trial → `dev_*`.
    #[command(name = "dev")]
    DevExp,
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

/// Expand a `default.toml` `run` entry into one or more dispatch commands.
fn commands_for_run_entry(name: &str) -> Result<Vec<Command>> {
    Ok(match name {
        "just_tx" => vec![Command::JustTxExp {
            path: None,
        }],
        "just_drop" => vec![Command::JustDropExp {
            path: None,
            sample_interval_ms: 0,
        }],
        "dev" => vec![Command::DevExp],
        "suite" => vec![Command::Suite],
        other => anyhow::bail!(
            "unknown experiment {other:?} in default.toml `run`; use just_tx / just_drop / dev (see config/README.md)"
        ),
    })
}

fn is_quiet_command(command: &Command) -> bool {
    !matches!(command, Command::Suite)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = FileConfig::embedded()?;
    install_postgres(cfg.postgres.clone());

    if args.help_benches {
        let mut cmd = Args::command();
        cmd.print_help()?;
        println!();
        return Ok(());
    }

    // Formal TR3000 path: no subcommand → execute `run` list from default.toml.
    if args.command.is_none() {
        let mut expanded = Vec::new();
        for name in &cfg.run {
            expanded.extend(commands_for_run_entry(name)?);
        }
        let quiet = expanded.iter().all(is_quiet_command);
        let log_default = if quiet { "warn" } else { "info" };
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_default))
            .init();
        info!(
            "embedded config v={} esp={} mac={} iface={} run={:?}",
            cfg.version, cfg.target.esp_ip, cfg.target.esp_mac, cfg.probe.iface, cfg.run
        );
        for name in &cfg.run {
            info!("run experiment {name}");
            for cmd in commands_for_run_entry(name)? {
                dispatch_command(&cfg, &args, cmd).await?;
            }
        }
        return Ok(());
    }

    let command = args.command.clone().expect("subcommand present");
    let log_default = if is_quiet_command(&command) {
        "warn"
    } else {
        "info"
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(log_default)).init();

    if matches!(command, Command::Serve { .. }) {
        let Command::Serve { http_listen } = command else {
            unreachable!();
        };
        return serve::run(http_listen).await;
    }

    info!(
        "embedded config v={} esp={} mac={} iface={} run={:?}",
        cfg.version, cfg.target.esp_ip, cfg.target.esp_mac, cfg.probe.iface, cfg.run
    );
    dispatch_command(&cfg, &args, command).await
}

async fn dispatch_command(cfg: &FileConfig, args: &Args, command: Command) -> Result<()> {
    if matches!(command, Command::JustDrop) {
        return bench::run_just_drop(
            cfg,
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
    if let Command::JustDropExp {
        path,
        sample_interval_ms,
    } = &command
    {
        let paths = match path {
            Some(p) => vec![bench::PpsPath::parse(p)?],
            None => Vec::new(),
        };
        let sample_ms = if *sample_interval_ms == 0 {
            cfg.just_drop.sample_interval_ms
        } else {
            *sample_interval_ms
        };
        return bench::run_just_drop_pps(
            cfg,
            bench::JustDropPpsOpts {
                paths,
                smoke: args.smoke,
                formal: args.formal || cfg.just_drop.formal,
                no_pg: args.no_pg,
                window_secs: args.window_secs,
                sample_interval_ms: sample_ms,
                rounds: args.rounds,
            },
        )
        .await;
    }
    if let Command::JustTxExp { path } = &command {
        let paths = match path {
            Some(p) => vec![bench::PpsPath::parse(p)?],
            None => Vec::new(),
        };
        let mut harness = open_harness(cfg).await?;
        return bench::run_just_tx(
            cfg,
            &mut harness,
            bench::JustTxOpts {
                paths,
                smoke: args.smoke,
                formal: args.formal || cfg.just_tx.formal,
                no_pg: args.no_pg,
                window_secs: args.window_secs,
                rounds: args.rounds,
                tables: bench::pg_dsn::TableSet::JustTx,
            },
        )
        .await;
    }
    if let Command::DevExp = &command {
        let mut harness = open_harness(cfg).await?;
        return bench::run_just_tx(
            cfg,
            &mut harness,
            bench::JustTxOpts {
                paths: Vec::new(), // ignored — hardcoded XDP
                smoke: false,
                formal: false,
                no_pg: true, // console-only; never touch Postgres
                window_secs: None,
                rounds: None,
                tables: bench::pg_dsn::TableSet::Dev,
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
    let mut harness = open_harness(cfg).await?;
    match command {
        Command::Serve { .. } => unreachable!(),
        Command::All => {
            return bench::run_all_benches(&mut harness, cfg, drop_opts).await;
        }
        Command::DropXdp => {
            ensure_bench_profile(&harness, cfg, "drop-xdp").await?;
            return bench::run_drop_xdp(&mut harness, drop_opts).await;
        }
        Command::DropHal => {
            ensure_bench_profile(&harness, cfg, "drop-hal").await?;
            return bench::run_drop_hal(&mut harness, drop_opts).await;
        }
        Command::Echo => {
            ensure_bench_profile(&harness, cfg, "echo").await?;
            return bench::run_echo(&mut harness, args.smoke, args.formal).await;
        }
        Command::WlRtt => {
            ensure_bench_profile(&harness, cfg, "wl-rtt").await?;
            return bench::run_wl_rtt(&mut harness, args.smoke, args.formal).await;
        }
        Command::WlLoss | Command::BenchLoss => {
            ensure_bench_profile(&harness, cfg, "wl-loss").await?;
            return bench::run_wl_loss(&mut harness, args.smoke, args.formal).await;
        }
        Command::RateRtt | Command::Bench => {
            ensure_bench_profile(&harness, cfg, "rate-rtt").await?;
            return bench::run_rate_rtt(&mut harness, args.smoke, args.formal).await;
        }
        Command::RateLoss => {
            ensure_bench_profile(&harness, cfg, "rate-loss").await?;
            return bench::run_rate_loss(&mut harness, args.smoke, args.formal).await;
        }
        Command::JustDrop
        | Command::JustDropExp { .. }
        | Command::JustTxExp { .. }
        | Command::DevExp => {
            unreachable!("handled earlier")
        }
        Command::Suite => {}
    }
    let report = run_all(cfg, &mut harness).await?;

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
