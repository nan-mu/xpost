//! Shared drop-bench window + probe/DUT throughput diagnostics.

use std::time::Duration;

use log::warn;
use test_common::drop_stats::{self, DropStats};

use super::udp_blast;
use crate::harness::Harness;

pub const WINDOW: Duration = Duration::from_secs(40);
/// Reach `CAP_PKTS` at ~3–4k effective DUT pps (WiFi loss); host TX may be ~10k pps.
pub const SMOKE_WINDOW: Duration = Duration::from_secs(40);
pub const FORMAL_ROUNDS: usize = 3;
pub const SMOKE_ROUNDS: usize = 1;

#[derive(Debug, Clone, Copy)]
pub struct DropOpts {
    pub smoke: bool,
    pub formal: bool,
    pub no_pg: bool,
    pub window_secs: Option<u64>,
    pub rounds: Option<usize>,
}

impl DropOpts {
    pub fn new(
        smoke: bool,
        formal: bool,
        no_pg: bool,
        window_secs: Option<u64>,
        rounds: Option<usize>,
    ) -> Self {
        Self {
            smoke,
            formal,
            no_pg,
            window_secs,
            rounds,
        }
    }

    pub fn window(&self) -> Duration {
        self.window_secs
            .map(Duration::from_secs)
            .unwrap_or_else(|| if self.smoke { SMOKE_WINDOW } else { WINDOW })
    }

    pub fn rounds(&self) -> usize {
        self.rounds.unwrap_or_else(|| {
            if self.smoke {
                SMOKE_ROUNDS
            } else {
                FORMAL_ROUNDS
            }
        })
    }
}

/// RESET (no reply) then wait for async firmware reset + ctrl QUERY sanity check.
pub async fn reset_with_settle(
    harness: &Harness,
    ctrl: std::net::Ipv4Addr,
    stop: &super::stop::Stop,
) -> anyhow::Result<()> {
    use std::time::Duration;

    harness
        .drop_stats_reset(ctrl)
        .await
        .map_err(|e| anyhow::anyhow!("drop stats RESET: {e:#}"))?;
    eprintln!("  RESET sent → settle {}ms …", drop_stats::RESET_SETTLE_MS);
    stop.sleep_or(Duration::from_millis(drop_stats::RESET_SETTLE_MS))
        .await;
    let stats = harness
        .drop_stats_query(ctrl)
        .await
        .map_err(|e| anyhow::anyhow!("post-RESET ctrl QUERY: {e:#}"))?;
    eprintln!(
        "  post-RESET QUERY ok  pkts_drop={}  t_us={}..{}",
        stats.drop_count, stats.t_first_us, stats.t_last_us
    );
    Ok(())
}

fn stats_ready(stats: &DropStats) -> bool {
    stats.drop_count >= drop_stats::CAP_PKTS
}

fn better_stats(a: &DropStats, b: &DropStats) -> DropStats {
    if b.drop_count > a.drop_count || (b.drop_count == a.drop_count && b.t_last_us > a.t_last_us) {
        *b
    } else {
        *a
    }
}

/// After blast: drain WiFi tail, wait DUT idle watchdog slices, QUERY each round.
pub async fn query_after_watchdog(
    harness: &Harness,
    ctrl: std::net::Ipv4Addr,
    stop: &super::stop::Stop,
) -> anyhow::Result<DropStats> {
    use std::time::Duration;

    let drain = Duration::from_secs(drop_stats::POST_BLAST_DRAIN_SECS);
    eprintln!(
        "  post-blast drain {}s (WiFi / driver queue) …",
        drain.as_secs()
    );
    stop.sleep_or(drain).await;

    let period = Duration::from_micros(drop_stats::watchdog_query_period_us());
    let mut last_ok: Option<DropStats> = None;
    let mut last_err = None;
    for round in 0..drop_stats::WATCHDOG_QUERY_ROUNDS {
        eprintln!(
            "  watchdog wait {}/{} (+{:.1}s) …",
            round + 1,
            drop_stats::WATCHDOG_QUERY_ROUNDS,
            period.as_secs_f64()
        );
        stop.sleep_or(period).await;
        match harness.drop_stats_query(ctrl).await {
            Ok(stats) => {
                eprintln!(
                    "  QUERY pkts_drop={}  t_us={}..{}",
                    stats.drop_count, stats.t_first_us, stats.t_last_us
                );
                last_ok = Some(match last_ok {
                    Some(prev) => better_stats(&prev, &stats),
                    None => stats,
                });
                if stats_ready(&stats) {
                    eprintln!("  CAP_PKTS reached → done");
                    return Ok(stats);
                }
            }
            Err(e) => {
                eprintln!(
                    "  QUERY round {}/{} timeout",
                    round + 1,
                    drop_stats::WATCHDOG_QUERY_ROUNDS
                );
                last_err = Some(e);
            }
        }
    }
    if let Some(stats) = last_ok {
        if stats.drop_count < drop_stats::CAP_PKTS {
            eprintln!(
                "  pkts_drop={} < CAP {} after watchdog budget",
                stats.drop_count,
                drop_stats::CAP_PKTS
            );
        }
        if stats.drop_count == 0 {
            warn!("drop stats QUERY ok but pkts_drop=0 after full watchdog budget");
        }
        return Ok(stats);
    }
    Err(last_err
        .map(|e| anyhow::anyhow!("{e:#}"))
        .unwrap_or_else(|| anyhow::anyhow!("drop stats QUERY budget exhausted")))
}

/// After blast: STOP (disable path + seal `t_last`), then QUERY.
pub async fn query_after_stop(
    harness: &Harness,
    ctrl: std::net::Ipv4Addr,
) -> anyhow::Result<DropStats> {
    harness
        .drop_stats_stop(ctrl)
        .await
        .map_err(|e| anyhow::anyhow!("drop stats STOP: {e:#}"))?;
    harness
        .drop_stats_query(ctrl)
        .await
        .map_err(|e| anyhow::anyhow!("drop stats QUERY: {e:#}"))
}

/// Compare host UDP blast vs DUT drop_stats to spot path bottlenecks.
pub fn log_path_stats(label: &str, stats: &DropStats, blast: &udp_blast::BlastResult) {
    let host_pps = blast.host_pps();
    let dut_pps = stats.dut_pps();
    eprintln!(
        "  [{label}] host_pps={host_pps:.0}  tx_count={}  blast={}s",
        blast.tx_count,
        blast.elapsed_secs()
    );
    match dut_pps {
        Some(pps) => eprintln!(
            "  [{label}] dut_pps={pps:.0}  pkts_drop={}  measured={}  t_us={}..{}  (window {}..{} pkts)",
            stats.drop_count,
            stats.measured_pkts(),
            stats.t_first_us,
            stats.t_last_us,
            drop_stats::WARMUP_PKTS,
            drop_stats::CAP_PKTS.min(stats.drop_count),
        ),
        None => eprintln!(
            "  [{label}] dut_pps=n/a  pkts_drop={}  measured={}  t_us={}..{}",
            stats.drop_count,
            stats.measured_pkts(),
            stats.t_first_us,
            stats.t_last_us,
        ),
    }

    let ratio = if blast.tx_count > 0 {
        stats.measured_pkts() as f64 / blast.tx_count as f64
    } else {
        0.0
    };
    eprintln!(
        "  [{label}] dut/measured_host_tx={ratio:.3}  (measured=pkts in [{}..{}], host_tx={})",
        drop_stats::WARMUP_PKTS,
        drop_stats::CAP_PKTS.min(stats.drop_count),
        blast.tx_count,
    );

    if host_pps < 5_000.0 {
        warn!(
            "{label}: host UDP blast only {host_pps:.0} pps — probe may be CPU-limited on TR3000"
        );
    } else if ratio < 0.05 {
        warn!(
            "{label}: DUT measured {ratio:.1}% of host TX — check WiFi link, datapath, drop path enable"
        );
    } else if ratio > 1.05 {
        warn!("{label}: dut/measured_host_tx={ratio:.3} > 1 — iface may include non-bench traffic");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_defaults() {
        let o = DropOpts::new(true, false, false, None, None);
        assert_eq!(o.window(), SMOKE_WINDOW);
        assert_eq!(o.rounds(), 1);
    }

    #[test]
    fn window_override() {
        let o = DropOpts::new(true, false, false, Some(30), None);
        assert_eq!(o.window(), Duration::from_secs(30));
        assert_eq!(o.rounds(), 1);
    }

    #[test]
    fn formal_defaults() {
        let o = DropOpts::new(false, true, false, None, None);
        assert_eq!(o.window(), WINDOW);
        assert_eq!(o.rounds(), FORMAL_ROUNDS);
    }
}
