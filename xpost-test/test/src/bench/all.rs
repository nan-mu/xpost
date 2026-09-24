//! Run every formal bench in sequence (`test all`).

use std::time::Instant;

use anyhow::{Result, bail};
use log::warn;

use crate::config::FileConfig;
use crate::harness::Harness;

use super::drop_common::DropOpts;
use super::flash;
use super::wl_common::fmt_dur;
use super::{
    run_drop_hal, run_drop_xdp, run_echo, run_rate_loss, run_rate_rtt, run_wl_loss, run_wl_rtt,
};

struct Step {
    name: &'static str,
}

const STEPS: [Step; 7] = [
    Step { name: "drop-xdp" },
    Step { name: "drop-hal" },
    Step { name: "echo" },
    Step { name: "wl-rtt" },
    Step { name: "wl-loss" },
    Step { name: "rate-rtt" },
    Step { name: "rate-loss" },
];

pub async fn run_all(harness: &mut Harness, cfg: &FileConfig, drop_opts: DropOpts) -> Result<()> {
    eprintln!(
        "all {} benches  smoke={}  formal={}  window_secs={:?}  rounds={:?}",
        STEPS.len(),
        drop_opts.smoke,
        drop_opts.formal,
        drop_opts.window_secs,
        drop_opts.rounds,
    );
    let started = Instant::now();
    let mut failed = Vec::new();
    let mut flashed: Option<String> = None;

    for step in STEPS {
        eprintln!();
        eprintln!("=== {} ===", step.name);
        if let Err(e) = flash::ensure_profile(harness, cfg, step.name, &mut flashed).await {
            warn!("{} profile flash failed: {e:#}", step.name);
            failed.push(step.name);
            continue;
        }
        let err = match step.name {
            "drop-xdp" => run_drop_xdp(harness, drop_opts).await.err(),
            "drop-hal" => run_drop_hal(harness, drop_opts).await.err(),
            "echo" => run_echo(harness, drop_opts.smoke, drop_opts.formal)
                .await
                .err(),
            "wl-rtt" => run_wl_rtt(harness, drop_opts.smoke, drop_opts.formal)
                .await
                .err(),
            "wl-loss" => run_wl_loss(harness, drop_opts.smoke, drop_opts.formal)
                .await
                .err(),
            "rate-rtt" => run_rate_rtt(harness, drop_opts.smoke, drop_opts.formal)
                .await
                .err(),
            "rate-loss" => run_rate_loss(harness, drop_opts.smoke, drop_opts.formal)
                .await
                .err(),
            _ => unreachable!(),
        };
        if let Some(e) = err {
            warn!("{} failed: {e:#}", step.name);
            failed.push(step.name);
        }
    }

    eprintln!();
    if failed.is_empty() {
        eprintln!("all done  elapsed {}", fmt_dur(started.elapsed()));
        Ok(())
    } else {
        eprintln!(
            "all done with failures ({}/{})  elapsed {}",
            failed.len(),
            STEPS.len(),
            fmt_dur(started.elapsed())
        );
        for name in &failed {
            eprintln!("  failed: {name}");
        }
        bail!("all: {} bench(es) failed", failed.len());
    }
}
