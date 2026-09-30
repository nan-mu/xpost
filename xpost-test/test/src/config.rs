//! Compile-time TOML for the `xpost-test` binary:
//! `config/default.toml` + `just_tx.toml` + `just_drop.toml`.
//!
//! Files are embedded at compile time; after editing, rebuild and scp to TR3000.
//! Formal collection is always:
//!
//! ```bash
//! ./xpost-test
//! ```
//!
//! No env vars, no CLI args. `default.toml` `run = [...]` selects **experiment
//! names** only; steps are hard-coded in Rust, not split across TOML.
//!
//! # `run` values
//!
//! | Name | What | Postgres tables |
//! |------|------|-----------------|
//! | `just_drop` | Formal HAL + XDP: `kinds` includes `c` (full-rate drop series) and `a`/`g`/`t` (fixed pps) | `just_drop_config`/`data`; `just_drop_run` + agg/gauge/trace_summary |
//! | `just_tx` | Formal HAL + XDP: `kinds` `c` (sweep / ~5% OP RTT) and `a`/`g`/`t` (fixed pps) | `just_tx_config`/`data`; `just_tx_run` + agg/gauge/trace_summary |
//! | `dev` | Flame Trace: XDP × 100 pps × 2s × 1; `/tmp/xpost-dev-flame.report`; no DB | — |
//!
//! `dev` is fully hard-coded (no separate TOML); never touches Postgres /
//! `just_tx_*` / `just_drop_*`.
//!
//! Formal experiments with `formal`: `DROP CASCADE` same-named tables then
//! recreate. DDL lives under `xpost-test/sql/`.
//!
//! ```toml
//! run = ["dev"]           # flame Trace short capture
//! # run = ["just_tx"]     # formal
//! # run = ["just_drop", "just_tx"]
//! ```
//!
//! # Other files
//!
//! - `default.toml`: `run`, postgres, agent, DUT, probe, profiles
//! - `just_drop.toml` / `just_tx.toml`: `arms` / `kinds` (`c|a|g|t`) /
//!   `fixed_pps` / `trace_pps` / `instr_rounds` / `instr_window_secs`
//!
//! # Functional suite (optional)
//!
//! `./xpost-test suite` runs fixed `test0`. `./xpost-test dev` is also available.

use std::{net::Ipv4Addr, str::FromStr, sync::OnceLock};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const EMBEDDED_DEFAULT: &str = include_str!("../../config/default.toml");
const EMBEDDED_JUST_TX: &str = include_str!("../../config/just_tx.toml");
const EMBEDDED_JUST_DROP: &str = include_str!("../../config/just_drop.toml");

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
    /// Ordered benches for bare `./xpost-test`.
    #[serde(default = "default_run")]
    pub run: Vec<String>,
    pub postgres: PostgresConfig,
    pub probe: ProbeConfig,
    #[serde(rename = "dut")]
    pub target: TargetConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub serve: ServeConfig,
    #[serde(default)]
    pub profiles: ProfilesConfig,
    #[serde(default)]
    pub just_drop: BenchPpsConfig,
    #[serde(default)]
    pub just_tx: BenchPpsConfig,
    #[serde(default)]
    pub tests: Vec<TestEntry>,
}

fn default_version() -> String {
    "0".into()
}

fn default_run() -> Vec<String> {
    vec!["dev".into()]
}

/// Fixed functional suite for `./xpost-test suite` (not selected via `run`).
fn default_suite_tests() -> Vec<TestEntry> {
    vec![TestEntry {
        name: "test0".into(),
        enabled: true,
        cases: vec![
            CaseConfig {
                id: "drop".into(),
                src_ip: Ipv4Addr::new(10, 0, 0, 1),
                dst_ip: None,
                expect_echo: false,
                expect_bounce: false,
            },
            CaseConfig {
                id: "tx".into(),
                src_ip: Ipv4Addr::new(10, 0, 0, 2),
                dst_ip: None,
                expect_echo: false,
                expect_bounce: true,
            },
        ],
    }]
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
    #[serde(default)]
    pub just_drop: Option<String>,
    #[serde(default)]
    pub just_drop_hal: Option<String>,
    #[serde(default)]
    pub just_tx: Option<String>,
    #[serde(default)]
    pub just_tx_hal: Option<String>,
}

/// Shared knobs for PPS / RTT arms (rounds, formal, …).
#[derive(Debug, Clone, Deserialize)]
pub struct BenchPpsConfig {
    #[serde(default = "default_true")]
    pub formal: bool,
    #[serde(default = "default_pps_rounds")]
    pub rounds: usize,
    #[serde(default)]
    pub window_secs: Option<u64>,
    #[serde(default = "default_sample_ms")]
    pub sample_interval_ms: u64,
    #[serde(default = "default_pps_arms")]
    pub arms: Vec<String>,
    /// `c` / `a` / `g` / `t` for just_drop and just_tx formal matrices.
    #[serde(default = "default_kinds_c")]
    pub kinds: Vec<String>,
    /// Fixed pps for Aggregate / Gauge (A/G).
    #[serde(default)]
    pub fixed_pps: Option<u64>,
    /// Fixed pps for Trace (lower than `fixed_pps`).
    #[serde(default)]
    pub trace_pps: Option<u64>,
    /// Rounds for A/G/T (Counter still uses `rounds` / sweep trials).
    #[serde(default)]
    pub instr_rounds: Option<usize>,
    /// Window seconds for A/G/T.
    #[serde(default)]
    pub instr_window_secs: Option<u64>,
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
fn default_kinds_c() -> Vec<String> {
    vec!["c".into()]
}

impl Default for BenchPpsConfig {
    fn default() -> Self {
        Self {
            formal: true,
            rounds: default_pps_rounds(),
            window_secs: None,
            sample_interval_ms: default_sample_ms(),
            arms: default_pps_arms(),
            kinds: default_kinds_c(),
            fixed_pps: None,
            trace_pps: None,
            instr_rounds: None,
            instr_window_secs: None,
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
    #[serde(default)]
    pub vip_ip: Option<Ipv4Addr>,
    pub esp_mac: String,
    #[serde(default = "default_echo_port")]
    pub echo_port: u16,
    #[serde(default = "default_reply_port")]
    pub reply_port: u16,
    #[serde(default = "default_stim_sport")]
    pub stim_sport: u16,
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
    #[serde(default)]
    pub dst_ip: Option<Ipv4Addr>,
    #[serde(default)]
    pub expect_echo: bool,
    #[serde(default)]
    pub expect_bounce: bool,
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
    #[serde(default)]
    serve: ServeConfig,
    #[serde(default)]
    profiles: ProfilesConfig,
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

impl FileConfig {
    /// Load and merge the compile-time TOMLs.
    pub fn embedded() -> Result<Self> {
        let def: DefaultFile =
            toml::from_str(EMBEDDED_DEFAULT).context("parse config/default.toml")?;
        let tx: JustTxFile =
            toml::from_str(EMBEDDED_JUST_TX).context("parse config/just_tx.toml")?;
        let drop: JustDropFile =
            toml::from_str(EMBEDDED_JUST_DROP).context("parse config/just_drop.toml")?;

        let cfg = Self {
            version: def.version,
            run: def.run,
            postgres: def.postgres,
            probe: def.probe,
            target: def.target,
            agent: def.agent,
            serve: def.serve,
            profiles: def.profiles,
            just_drop: drop.bench,
            just_tx: tx.bench,
            tests: default_suite_tests(),
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
                "just_tx" | "just_drop" | "dev" | "suite" => {}
                "just-tx" | "just-drop" | "just-tx-pps" | "just-tx-rtt" | "just-drop-pps" => {
                    bail!(
                        "default.toml `run` entry {name:?} is not an experiment name; use just_tx / just_drop / dev (see `config` module docs)"
                    )
                }
                other => bail!(
                    "unknown experiment {other:?} in default.toml `run`; use just_tx / just_drop / dev (see `config` module docs)"
                ),
            }
        }
        if self.probe.iface.is_empty() {
            bail!("probe.iface must be set");
        }
        if self.postgres.host.is_empty() {
            bail!("postgres.host must be set");
        }
        parse_mac(&self.target.esp_mac).context("dut.esp_mac")?;
        if let Some(ref m) = self.probe.src_mac {
            parse_mac(m).context("probe.src_mac")?;
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

    fn from_str(_s: &str) -> Result<Self> {
        bail!("FileConfig::from_str removed; use FileConfig::embedded()")
    }
}
