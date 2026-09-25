//! TOML configuration for the probe binary (compile-time embed + optional parse helpers).

use std::{net::Ipv4Addr, str::FromStr};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Baked-in config from [`config/test.toml`](../../config/test.toml).
pub const EMBEDDED_CONFIG_TOML: &str = include_str!("../../config/test.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    pub probe: ProbeConfig,
    pub target: TargetConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub serve: ServeConfig,
    #[serde(default)]
    pub profiles: ProfilesConfig,
    /// Bare `./xpost-test` (no subcommand) runs this bench.
    #[serde(default = "default_default_bench")]
    pub default_bench: String,
    #[serde(default)]
    pub just_drop_pps: JustDropPpsConfig,
    #[serde(default)]
    pub just_tx_pps: JustDropPpsConfig,
    #[serde(default)]
    pub tests: Vec<TestEntry>,
}

fn default_default_bench() -> String {
    "just-drop-pps".into()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentConfig {
    /// Mac `xpost-agent` listen address, e.g. `192.168.110.96:9240`.
    #[serde(default)]
    pub endpoint: Option<String>,
}

impl AgentConfig {
    /// Mac agent address: `XPOST_AGENT_ENDPOINT` env overrides embedded `[agent].endpoint`.
    pub fn resolved_endpoint(&self) -> Option<String> {
        std::env::var("XPOST_AGENT_ENDPOINT")
            .ok()
            .or_else(|| self.endpoint.clone())
            .filter(|s| !s.is_empty())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServeConfig {
    #[serde(default = "default_http_listen")]
    pub http_listen: String,
}

fn default_http_listen() -> String {
    "0.0.0.0:19081".into()
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProfilesConfig {
    #[serde(default)]
    pub drop_hal: Option<String>,
    #[serde(default)]
    pub drop_xdp: Option<String>,
    #[serde(default)]
    pub firmware: Option<String>,
    /// Profile id for just_drop firmware (+ optional xdp.elf on agent).
    #[serde(default)]
    pub just_drop: Option<String>,
    /// Profile id for just_drop_hal firmware.
    #[serde(default)]
    pub just_drop_hal: Option<String>,
    /// Profile id for just_tx (just_drop firmware + just_tx.elf).
    #[serde(default)]
    pub just_tx: Option<String>,
    /// Profile id for just_tx_hal firmware.
    #[serde(default)]
    pub just_tx_hal: Option<String>,
}

/// Compile-time defaults for `just-drop-pps` / bare `./xpost-test`.
#[derive(Debug, Clone, Deserialize)]
pub struct JustDropPpsConfig {
    /// When true, write schema `formal` and empty tables before the run.
    #[serde(default = "default_true")]
    pub formal: bool,
    #[serde(default = "default_pps_rounds")]
    pub rounds: usize,
    #[serde(default)]
    pub window_secs: Option<u64>,
    #[serde(default = "default_sample_ms")]
    pub sample_interval_ms: u64,
    /// Arms in order, e.g. `["hal", "xdp"]`.
    #[serde(default = "default_pps_arms")]
    pub arms: Vec<String>,
}

fn default_true() -> bool {
    true
}
fn default_pps_rounds() -> usize {
    3
}
fn default_sample_ms() -> u64 {
    1000
}
fn default_pps_arms() -> Vec<String> {
    vec!["hal".into(), "xdp".into()]
}

impl Default for JustDropPpsConfig {
    fn default() -> Self {
        Self {
            formal: true,
            rounds: default_pps_rounds(),
            window_secs: None,
            sample_interval_ms: default_sample_ms(),
            arms: default_pps_arms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeConfig {
    pub iface: String,
    #[serde(default = "default_pair_timeout_ms")]
    pub pair_timeout_ms: u64,
    #[serde(default = "default_report_path")]
    pub report_path: String,
    /// Optional override for Ethernet source MAC (aa:bb:...).
    #[serde(default)]
    pub src_mac: Option<String>,
}

fn default_pair_timeout_ms() -> u64 {
    2000
}

fn default_report_path() -> String {
    "/tmp/test-report.json".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct TargetConfig {
    pub esp_ip: Ipv4Addr,
    /// VIP (A) for XDP L2 bounce tests; optional when only running firewall_echo.
    #[serde(default)]
    pub vip_ip: Option<Ipv4Addr>,
    pub esp_mac: String,
    #[serde(default = "default_echo_port")]
    pub echo_port: u16,
    #[serde(default = "default_reply_port")]
    pub reply_port: u16,
    /// RAW stimulus UDP source port. Must differ from `reply_port`: `fw_hash_bounce`
    /// swaps ports, and TC classifies bounce as `sport==echo && dport!=reply`.
    #[serde(default = "default_stim_sport")]
    pub stim_sport: u16,
    /// UDP control port for MapCrudOp / HotLoadOp.
    #[serde(default = "default_ctrl_port")]
    pub ctrl_port: u16,
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

fn default_ctrl_port() -> u16 {
    8081
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestEntry {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub cases: Vec<CaseConfig>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct CaseConfig {
    pub id: String,
    pub src_ip: Ipv4Addr,
    /// Stimulus destination; omit to use `target.esp_ip` (B / STA).
    #[serde(default)]
    pub dst_ip: Option<Ipv4Addr>,
    /// Firewall / STA path: expect `FWSTIM`+seq echo on reply_port.
    #[serde(default)]
    pub expect_echo: bool,
    /// VIP path: expect same `FWSTIM`+seq payload bounced (XDP TX).
    #[serde(default)]
    pub expect_bounce: bool,
}

impl FileConfig {
    /// Load the TOML embedded at compile time.
    pub fn embedded() -> Result<Self> {
        Self::from_str(EMBEDDED_CONFIG_TOML).context("parse embedded config/test.toml")
    }

    fn validate(&self) -> Result<()> {
        if self.probe.iface.is_empty() {
            bail!("probe.iface must be set");
        }
        parse_mac(&self.target.esp_mac).context("target.esp_mac")?;
        if let Some(ref m) = self.probe.src_mac {
            parse_mac(m).context("probe.src_mac")?;
        }
        if self.tests.is_empty() {
            bail!("at least one [[tests]] entry required");
        }
        for t in &self.tests {
            if t.name.is_empty() {
                bail!("tests.name must be non-empty");
            }
            if t.enabled && t.cases.is_empty() {
                bail!("enabled test '{}' has no cases", t.name);
            }
            for c in &t.cases {
                if c.id.is_empty() {
                    bail!("case id must be non-empty in test '{}'", t.name);
                }
            }
        }
        Ok(())
    }

    pub fn esp_mac_bytes(&self) -> Result<[u8; 6]> {
        parse_mac(&self.target.esp_mac)
    }

    pub fn src_mac_override(&self) -> Result<Option<[u8; 6]>> {
        match &self.probe.src_mac {
            Some(s) => Ok(Some(parse_mac(s)?)),
            None => Ok(None),
        }
    }
}

/// Parse `aa:bb:cc:dd:ee:ff` or `aa-bb-cc-dd-ee-ff`.
pub fn parse_mac(s: &str) -> Result<[u8; 6]> {
    let sep = if s.contains(':') {
        ':'
    } else if s.contains('-') {
        '-'
    } else {
        bail!("MAC must use : or - separators: {s}");
    };
    let parts: Vec<&str> = s.split(sep).collect();
    if parts.len() != 6 {
        bail!("MAC must have 6 octets: {s}");
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] =
            u8::from_str_radix(p, 16).with_context(|| format!("bad MAC octet '{p}' in {s}"))?;
    }
    Ok(out)
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

impl FromStr for FileConfig {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }
}
