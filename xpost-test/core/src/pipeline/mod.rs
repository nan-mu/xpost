//! Stimulus / TC harness plumbing shared by exams.

mod harness;
mod raw_tx;
pub mod status;

pub use harness::Harness;
pub use raw_tx::{RawTx, iface_ipv4, iface_mac};
pub use status::{BudgetEta, READY_BUDGET, RoundUi};

use anyhow::{Context, Result};
use log::info;

use crate::config::{FileConfig, format_mac};

/// Open AF_PACKET + TC harness for just_tx / dev.
pub async fn open_harness(cfg: &FileConfig) -> Result<Harness> {
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
