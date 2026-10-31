//! Compile-time TOML for the `xpost-test` binary:
//! `config/default.toml` + `just_tx.toml` + `just_drop.toml` + `firewall.toml`
//! + `exec_bench.toml`.
//!
//! Files are embedded at compile time; after editing, rebuild and scp to TR3000.
//! The only runtime entry is bare:
//!
//! ```bash
//! ./xpost-test
//! ```
//!
//! No env vars, no CLI args. `default.toml` `run = [...]` selects **experiment
//! names** only; steps are hard-coded in Rust.
//!
//! # `run` values
//!
//! | Name | What | Postgres tables |
//! |------|------|-----------------|
//! | `just_drop` | Formal XDP PPS grid + Trace | `exam` + `just_drop_*` + `xdp_*` / `trace_event` |
//! | `just_tx` | Formal XDP pps grid RTT + Trace | `exam` + `just_tx_config`(sent/ok) / `just_tx_data` + perf |
//! | `firewall` | Hash blacklist size×hit/miss PPS grid + Trace | `exam` + `firewall_*` + perf |
//! | `exec_bench` | Exec ingress function-call A/B (eBPF + HAL) | `exam` + `exec_bench_*` + perf |
//! | `dev` | HAL smoke: just_tx/just_drop/firewall `@100pps×2s`; no XDP, no DB | — |
//! | `formal-clear` | `TRUNCATE exam CASCADE` | wipe only |
//!
//! `dev` is fully hard-coded (no separate TOML); never touches Postgres.
//!
//! Formal DDL: `sql/formal.sql` + `just_tx.sql` / `just_drop.sql` / `firewall.sql`
//! / `exec_bench.sql` (open-run never DROP/TRUNCATE). Wipe: `run = ["formal-clear"]`.
//!
//! ```toml
//! run = ["dev"]
//! # run = ["just_tx"]
//! # run = ["just_drop", "just_tx"]
//! # run = ["firewall"]
//! # run = ["exec_bench"]
//! # run = ["formal-clear"]
//! ```

use std::{net::Ipv4Addr, sync::OnceLock};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const EMBEDDED_DEFAULT: &str = include_str!("../../config/default.toml");
const EMBEDDED_JUST_TX: &str = include_str!("../../config/just_tx.toml");
const EMBEDDED_JUST_DROP: &str = include_str!("../../config/just_drop.toml");
const EMBEDDED_FIREWALL: &str = include_str!("../../config/firewall.toml");
const EMBEDDED_EXEC_BENCH: &str = include_str!("../../config/exec_bench.toml");

static POSTGRES: OnceLock<PostgresConfig> = OnceLock::new();

/// Install Postgres settings from embedded config (call once at process start).
pub fn install_postgres(cfg: PostgresConfig) {
    let _ = POSTGRES.set(cfg);
}

/// Compile-time Postgres DSN pieces (after [`install_postgres`]).
pub fn postgres() -> &'static PostgresConfig {
    POSTGRES
        .get()
        .expect("postgres config: call install_postgres after FileConfig::embedded")
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileConfig {
    #[serde(default = "default_version")]
    pub version: String,
    /// Ordered experiments for bare `./xpost-test`.
    #[serde(default = "default_run")]
    pub run: Vec<String>,
    pub postgres: PostgresConfig,
    pub probe: ProbeConfig,
    #[serde(rename = "dut")]
    pub target: TargetConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub just_drop: BenchPpsConfig,
    #[serde(default)]
    pub just_tx: BenchPpsConfig,
    #[serde(default)]
    pub firewall: FirewallConfig,
    #[serde(default)]
    pub exec_bench: ExecBenchConfig,
}

fn default_version() -> String {
    "0".into()
}

fn default_run() -> Vec<String> {
    vec!["dev".into()]
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostgresConfig {
    pub host: String,
    #[serde(default = "default_pg_port")]
    pub port: u16,
    pub user: String,
    pub password: String,
    #[serde(default = "default_pg_dbname")]
    pub dbname: String,
    #[serde(default = "default_formal_schema")]
    pub formal_schema: String,
}

fn default_pg_port() -> u16 {
    5432
}
fn default_pg_dbname() -> String {
    "postgres".into()
}
fn default_formal_schema() -> String {
    "formal".into()
}

impl PostgresConfig {
    pub fn dsn(&self) -> String {
        format!(
            "host={} port={} dbname={} user={} password={} connect_timeout=10 sslmode=disable",
            self.host, self.port, self.dbname, self.user, self.password
        )
    }
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

/// Knobs for formal PPS / RTT arms (`just_tx.toml` / `just_drop.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct BenchPpsConfig {
    #[serde(default = "default_true")]
    pub formal: bool,
    #[serde(default = "default_pps_arms")]
    pub arms: Vec<String>,
}

/// Knobs for hash-map firewall formal (`firewall.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct FirewallConfig {
    #[serde(default = "default_true")]
    pub formal: bool,
    #[serde(default = "default_pps_arms")]
    pub arms: Vec<String>,
    #[serde(default = "default_fw_blacklist_size")]
    pub blacklist_size: u32,
    #[serde(default = "default_fw_legal_packets")]
    pub legal_packets: u32,
}

fn default_true() -> bool {
    true
}
fn default_pps_arms() -> Vec<String> {
    vec!["hal".into(), "xdp".into()]
}
fn default_fw_blacklist_size() -> u32 {
    200
}
fn default_fw_legal_packets() -> u32 {
    1024
}

impl Default for BenchPpsConfig {
    fn default() -> Self {
        Self {
            formal: true,
            arms: default_pps_arms(),
        }
    }
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            formal: true,
            arms: default_pps_arms(),
            blacklist_size: default_fw_blacklist_size(),
            legal_packets: default_fw_legal_packets(),
        }
    }
}

/// Knobs for exec_bench formal (`exec_bench.toml`).
#[derive(Debug, Clone, Deserialize)]
pub struct ExecBenchConfig {
    #[serde(default = "default_true")]
    pub formal: bool,
    #[serde(default = "default_exec_arms")]
    pub arms: Vec<String>,
}

fn default_exec_arms() -> Vec<String> {
    vec!["ebpf".into(), "hal".into()]
}

impl Default for ExecBenchConfig {
    fn default() -> Self {
        Self {
            formal: true,
            arms: default_exec_arms(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProbeConfig {
    pub iface: String,
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
    #[serde(default = "default_run")]
    run: Vec<String>,
    postgres: PostgresConfig,
    probe: ProbeConfig,
    #[serde(rename = "dut")]
    target: TargetConfig,
    #[serde(default)]
    agent: AgentConfig,
}

#[derive(Debug, Deserialize)]
struct JustTxFile {
    #[serde(flatten)]
    bench: BenchPpsConfig,
}

#[derive(Debug, Deserialize)]
struct JustDropFile {
    #[serde(flatten)]
    bench: BenchPpsConfig,
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
        let def: DefaultFile =
            toml::from_str(EMBEDDED_DEFAULT).context("parse config/default.toml")?;
        let just_tx: JustTxFile =
            toml::from_str(EMBEDDED_JUST_TX).context("parse config/just_tx.toml")?;
        let just_drop: JustDropFile =
            toml::from_str(EMBEDDED_JUST_DROP).context("parse config/just_drop.toml")?;
        let firewall: FirewallFile =
            toml::from_str(EMBEDDED_FIREWALL).context("parse config/firewall.toml")?;
        let exec_bench: ExecBenchFile =
            toml::from_str(EMBEDDED_EXEC_BENCH).context("parse config/exec_bench.toml")?;
        let cfg = Self {
            version: def.version,
            run: def.run,
            postgres: def.postgres,
            probe: def.probe,
            target: def.target,
            agent: def.agent,
            just_drop: just_drop.bench,
            just_tx: just_tx.bench,
            firewall: firewall.bench,
            exec_bench: exec_bench.bench,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.run.is_empty() {
            bail!("default.toml `run` must list at least one experiment");
        }
        for name in &self.run {
            match name.as_str() {
                "just_tx" | "just_drop" | "firewall" | "exec_bench" | "lookup" | "dev"
                | "formal-clear" | "case" | "hot_swap" => {}
                other => bail!(
                    "unknown experiment {other:?} in default.toml `run`; use just_tx / just_drop / firewall / exec_bench / lookup / dev / formal-clear / case / hot_swap"
                ),
            }
        }
        let n = self.firewall.blacklist_size;
        if n == 0 || n > 256 {
            bail!("firewall.toml blacklist_size {n} out of 1..=256");
        }
        let legal = self.firewall.legal_packets;
        if legal == 0 || legal > 1024 {
            bail!("firewall.toml legal_packets {legal} out of 1..=1024");
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
