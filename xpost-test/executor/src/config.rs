//! Embedded executor + agent settings from `config/default.toml`.

use anyhow::{Context, Result};
use serde::Deserialize;

pub const EMBEDDED_CONFIG_TOML: &str = include_str!("../../config/default.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutorConfig {
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub executor: ExecutorSection,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentSection {
    #[serde(default)]
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExecutorSection {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_http_listen")]
    pub http_listen: String,
    #[serde(default = "default_probe_bin", alias = "test_bin")]
    pub probe_bin: String,
}

fn default_listen() -> String {
    "0.0.0.0:19080".into()
}

fn default_http_listen() -> String {
    "0.0.0.0:19081".into()
}

fn default_probe_bin() -> String {
    "./xpost-test".into()
}

impl Default for ExecutorSection {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            http_listen: default_http_listen(),
            probe_bin: default_probe_bin(),
        }
    }
}

impl ExecutorConfig {
    pub fn embedded() -> Result<Self> {
        toml::from_str(EMBEDDED_CONFIG_TOML).context("parse embedded default.toml")
    }

    pub fn agent_endpoint(&self) -> String {
        if self.agent.endpoint.is_empty() {
            "127.0.0.1:9240".into()
        } else {
            self.agent.endpoint.clone()
        }
    }
}
