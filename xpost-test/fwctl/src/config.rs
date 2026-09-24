//! Defaults from embedded `config/test.toml`.

use serde::Deserialize;

const EMBEDDED: &str = include_str!("../../config/test.toml");

#[derive(Debug, Deserialize)]
struct Root {
    #[serde(default)]
    agent: AgentSection,
}

#[derive(Debug, Deserialize, Default)]
struct AgentSection {
    #[serde(default)]
    endpoint: String,
}

pub fn embedded_agent_endpoint() -> String {
    let root: Root = toml::from_str(EMBEDDED).expect("parse embedded test.toml");
    if root.agent.endpoint.is_empty() {
        "127.0.0.1:9240".into()
    } else {
        root.agent.endpoint
    }
}
