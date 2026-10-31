//! Formal / HAL-smoke exams driven by compile-time `config/`.

use anyhow::Context;

pub mod case;
pub mod hot_swap;
pub mod dev;
pub mod exec_bench;
pub mod firewall;
pub mod just_drop;
pub mod just_tx;
pub mod repeat;

pub use case::run_case;
pub use hot_swap::run_hot_swap;
pub use dev::run_dev;
pub use exec_bench::{ExecBenchOpts, run_exec_bench};
pub use firewall::{FirewallOpts, run_firewall};
pub use just_drop::{JustDropPpsOpts, run_just_drop_pps};
pub use just_tx::{JustTxOpts, run_just_tx};

/// Side-experiment rates for helper histograms. Not the main load grid.
pub const LOOKUP_RATES: &[f64] = &[400.0, 1400.0, 4000.0];

/// Set the lookup histogram gate on one `perf-lookup` firmware.
/// The image is released before the write. The caller enables it again.
pub async fn set_lookup_switch(agent: &crate::agent::AgentClient, on: bool) -> anyhow::Result<()> {
    agent
        .disable_xdp()
        .await
        .context("disable before lookup switch")?;
    let bit = if on { "1" } else { "0" };
    agent
        .update_map_batch("lookup_timing", &[("0".to_string(), bit.to_string())])
        .await
        .context("set lookup_timing")?;
    Ok(())
}
