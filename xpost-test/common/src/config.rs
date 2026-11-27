//! Compile-time TOML for the `xpost-test` binary:
//! `profile.toml` + `config/default.toml` + `firewall.toml`
//! + `exec_bench.toml`.
//!
//! Files are embedded at compile time; after editing, rebuild and scp to TR3000.
//! The only runtime entry is bare:
//!
//! ```bash
//! ./xpost-test
//! ```
//!
//! No env vars, no CLI args. `profile.toml` `run = [...]` selects **experiment
//! names** only; steps are hard-coded in Rust.
//!
//! Campaigns and the standalone EXB2 collector write Protocol V1.
//! Plan selections and the HAL smoke test need no database.
//! Database selections load `/root/.db` when needed.

use std::net::Ipv4Addr;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const EMBEDDED_DEFAULT: &str = include_str!("../../config/default.toml");
const EMBEDDED_PROFILE: &str = include_str!("../../profile.toml");
const EMBEDDED_FIREWALL: &str = include_str!("../../config/firewall.toml");
const EMBEDDED_EXEC_BENCH: &str = include_str!("../../config/exec_bench.toml");

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MediumStage {
    #[default]
    HalAot,
    JitRbpf,
}

impl MediumStage {
    pub const fn modes(self) -> [&'static str; 2] {
        match self {
            Self::HalAot => ["hal", "aot"],
            Self::JitRbpf => ["jit", "rbpf"],
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HalAot => "hal-aot",
            Self::JitRbpf => "jit-rbpf",
        }
    }

    pub const fn for_mode(mode: &str) -> &'static str {
        if matches!(mode.as_bytes(), b"hal" | b"aot") {
            Self::HalAot.as_str()
        } else {
            Self::JitRbpf.as_str()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    #[serde(default = "default_version")]
    pub version: String,
    /// Ordered experiments for bare `./xpost-test`.
    pub run: Vec<String>,
    #[serde(default)]
    pub medium_resume_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub medium_resume_experiment: Option<String>,
    #[serde(default)]
    pub medium_stage: MediumStage,
    pub probe: ProbeConfig,
    #[serde(rename = "dut")]
    pub target: TargetConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub firewall: FirewallConfig,
    #[serde(default)]
    pub exec_bench: ExecBenchConfig,
}

fn default_version() -> String {
    "0".into()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentConfig {
    /// Mac `xpost-agent` listen address, e.g. `192.168.110.96:9240`.
    #[serde(default)]
    pub endpoint: Option<String>,
}

impl AgentConfig {
    /// Embedded `[agent].endpoint` only (no env override on TR3000).
    pub fn resolved_endpoint(&self) -> Option<String> {
        self.endpoint.clone().filter(|s| !s.is_empty())
    }
}

/// Firewall blacklist size for the Protocol V1 collector.
#[derive(Debug, Clone, Deserialize)]
pub struct FirewallConfig {
    #[serde(default = "default_fw_blacklist_size")]
    pub blacklist_size: u32,
}

fn default_fw_blacklist_size() -> u32 {
    200
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            blacklist_size: default_fw_blacklist_size(),
        }
    }
}

/// EXB2 modes. Legacy `arms` remains a read alias.
#[derive(Debug, Clone, Deserialize)]
pub struct ExecBenchConfig {
    #[serde(default = "default_exec_modes", alias = "arms")]
    pub modes: Vec<String>,
    #[serde(default)]
    pub workloads: Vec<String>,
}

fn default_exec_modes() -> Vec<String> {
    ["hal", "aot", "jit", "rbpf"].map(str::to_owned).to_vec()
}

impl Default for ExecBenchConfig {
    fn default() -> Self {
        Self {
            modes: default_exec_modes(),
            workloads: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeConfig {
    pub iface: String,
    pub capture_rx_iface: Option<String>,
    #[serde(default)]
    pub src_mac: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TargetConfig {
    pub esp_ip: Ipv4Addr,
    pub vip_ip: Ipv4Addr,
    pub esp_mac: String,
    #[serde(default = "default_echo_port")]
    pub echo_port: u16,
    #[serde(default = "default_reply_port")]
    pub reply_port: u16,
    #[serde(default = "default_stim_sport")]
    pub stim_sport: u16,
}

fn default_echo_port() -> u16 {
    8080
}
fn default_reply_port() -> u16 {
    18080
}
fn default_stim_sport() -> u16 {
    18081
}

#[derive(Debug, Deserialize)]
struct DefaultFile {
    #[serde(default = "default_version")]
    version: String,
    probe: ProbeConfig,
    #[serde(rename = "dut")]
    target: TargetConfig,
    #[serde(default)]
    agent: AgentConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    run: Vec<String>,
    medium_resume_id: Option<uuid::Uuid>,
    medium_resume_experiment: Option<String>,
    #[serde(default)]
    medium_stage: MediumStage,
}

#[derive(Debug, Deserialize)]
struct FirewallFile {
    #[serde(flatten)]
    bench: FirewallConfig,
}

#[derive(Debug, Deserialize)]
struct ExecBenchFile {
    #[serde(flatten)]
    bench: ExecBenchConfig,
}

impl FileConfig {
    pub fn embedded() -> Result<Self> {
        let profile: ProfileFile =
            toml::from_str(EMBEDDED_PROFILE).context("parse profile.toml")?;
        let def: DefaultFile =
            toml::from_str(EMBEDDED_DEFAULT).context("parse config/default.toml")?;
        let firewall: FirewallFile =
            toml::from_str(EMBEDDED_FIREWALL).context("parse config/firewall.toml")?;
        let exec_bench: ExecBenchFile =
            toml::from_str(EMBEDDED_EXEC_BENCH).context("parse config/exec_bench.toml")?;
        let cfg = Self {
            version: def.version,
            run: profile.run,
            medium_resume_id: profile.medium_resume_id,
            medium_resume_experiment: profile.medium_resume_experiment,
            medium_stage: profile.medium_stage,
            probe: def.probe,
            target: def.target,
            agent: def.agent,
            firewall: firewall.bench,
            exec_bench: exec_bench.bench,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.run.is_empty() {
            bail!("profile.toml `run` must list at least one experiment");
        }
        if self.run.len() != 1
            && self.run.iter().any(|name| {
                matches!(
                    name.as_str(),
                    "medium"
                        | "medium-plan"
                        | "medium-preflight"
                        | "all"
                        | "all-plan"
                        | "all-preflight"
                        | "db-preflight"
                )
            })
        {
            bail!("campaign selections require a single-entry run list");
        }
        for name in &self.run {
            match name.as_str() {
                "all" | "all-plan" | "all-preflight" | "medium" | "medium-plan"
                | "medium-preflight" | "db-preflight" | "exec_bench" | "dev" => {}
                "just_tx" | "just_drop" | "firewall" => bail!(
                    "standalone {name} is retired; select all or medium for Protocol V1 network collection"
                ),
                "lookup" | "formal-clear" => bail!("run entry {name} is retired"),
                other => bail!(
                    "unknown experiment {other:?} in profile.toml `run`; use all / all-plan / all-preflight / medium / medium-plan / medium-preflight / db-preflight / exec_bench / dev"
                ),
            }
        }
        let n = self.firewall.blacklist_size;
        if n == 0 || n > 256 {
            bail!("firewall.toml blacklist_size {n} out of 1..=256");
        }
        Ok(())
    }

    pub fn esp_mac_bytes(&self) -> Result<[u8; 6]> {
        parse_mac(&self.target.esp_mac).context("dut.esp_mac")
    }

    pub fn src_mac_override(&self) -> Result<Option<[u8; 6]>> {
        match &self.probe.src_mac {
            Some(s) if !s.is_empty() => Ok(Some(parse_mac(s).context("probe.src_mac")?)),
            _ => Ok(None),
        }
    }
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let parts: Vec<_> = s.split(':').collect();
    if parts.len() != 6 {
        bail!("MAC must be 6 colon-separated hex octets, got {s:?}");
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16).with_context(|| format!("MAC octet {p:?}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn historical_database_selections_are_rejected_before_dispatch() {
        for selection in ["just_tx", "just_drop", "firewall", "lookup", "formal-clear"] {
            let mut cfg = super::FileConfig::embedded().unwrap();
            cfg.run = vec!["dev".into(), selection.into()];
            assert!(cfg.validate().unwrap_err().to_string().contains("retired"));
        }
    }

    #[test]
    fn retired_case_selections_are_rejected_before_dispatch() {
        for selection in ["hot_swap", "case-smoke"] {
            let mut cfg = super::FileConfig::embedded().unwrap();
            cfg.run = vec![selection.into()];
            assert!(
                cfg.validate()
                    .unwrap_err()
                    .to_string()
                    .contains("unknown experiment")
            );
        }
    }
}
