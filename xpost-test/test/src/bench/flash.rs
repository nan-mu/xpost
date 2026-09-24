//! Profile flash via Mac `xpost-agent` before `test all` benches.

use std::time::Duration;

use anyhow::{Context, Result};
use log::warn;

use crate::agent::AgentClient;
use crate::config::{FileConfig, ProfilesConfig};
use crate::harness::Harness;

use super::dut_ready;

const FLASH_IDLE_BUDGET: Duration = Duration::from_secs(180);

pub fn profile_for_bench<'a>(profiles: &'a ProfilesConfig, bench: &str) -> Option<&'a str> {
    match bench {
        "drop-xdp" => profiles.drop_xdp.as_deref(),
        "echo" | "wl-rtt" | "wl-loss" | "rate-rtt" | "rate-loss" => profiles.firmware.as_deref(),
        "drop-hal" => profiles.drop_hal.as_deref(),
        _ => None,
    }
}

pub async fn ensure_profile(
    harness: &Harness,
    cfg: &FileConfig,
    bench: &str,
    last: &mut Option<String>,
) -> Result<()> {
    let profile_id = match profile_for_bench(&cfg.profiles, bench) {
        Some(id) => id.to_string(),
        None => {
            warn!("no [profiles] mapping for {bench}; skip agent flash");
            return Ok(());
        }
    };
    if last.as_deref() == Some(profile_id.as_str()) {
        return Ok(());
    }

    let endpoint = cfg.agent.resolved_endpoint();
    let Some(endpoint) = endpoint else {
        warn!("no agent.endpoint; flash profile '{profile_id}' manually before {bench}");
        return Ok(());
    };

    eprintln!("flash profile {profile_id} via {endpoint} …");
    let agent = AgentClient::new(endpoint);
    agent.flash_profile(&profile_id).await.with_context(|| {
        format!(
            "FlashProfile {profile_id} (check Mac `cargo xtask agent`, \
                 LAN route to agent, or set XPOST_AGENT_ENDPOINT=<mac-ip>:9240)"
        )
    })?;
    agent
        .wait_idle(FLASH_IDLE_BUDGET)
        .await
        .context("wait agent idle after flash")?;
    dut_ready::wait_after_flash(harness, &profile_id).await?;
    *last = Some(profile_id);
    Ok(())
}
