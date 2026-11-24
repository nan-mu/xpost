//! Fixed medium plan. Keep rates and repeats inside each firmware group.

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{agent::AgentClient, config::FileConfig};

const MODES: [&str; 4] = ["hal", "aot", "jit", "rbpf"];
const NETWORK: [&str; 3] = ["firewall", "just_drop", "just_tx"];
const EXEC: [&str; 7] = [
    "noop",
    "prime",
    "log2",
    "copy",
    "fletcher32",
    "bsort",
    "strcmp",
];

#[derive(Serialize)]
struct Group {
    firmware: String,
    cells: Vec<Cell>,
}

#[derive(Serialize)]
struct Cell {
    experiment: &'static str,
    mode: &'static str,
    variant: &'static str,
    conditions: Value,
}

fn plan() -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut add = |firmware, cell| {
        if let Some(group) = groups.iter_mut().find(|group| group.firmware == firmware) {
            group.cells.push(cell);
        } else {
            groups.push(Group {
                firmware,
                cells: vec![cell],
            });
        }
    };
    for experiment in NETWORK {
        let family = experiment.replace('_', "-");
        for mode in MODES {
            for variant in ["metrics", "trace"] {
                let firmware = match (mode, variant) {
                    ("hal", _) => format!("{family}-hal"),
                    ("rbpf", "metrics") => format!("{family}-rbpf"),
                    ("rbpf", "trace") => format!("{family}-rbpf-trace"),
                    _ => format!("{family}-{variant}"),
                };
                let conditions = if variant == "metrics" {
                    json!({ "pps": [400,1400,4000], "window_s": 5, "repeats": 2 })
                } else if experiment == "firewall" {
                    json!({ "probes": ["hit","miss"] })
                } else {
                    json!({ "probes": ["tagged"] })
                };
                add(
                    firmware,
                    Cell {
                        experiment,
                        mode,
                        variant,
                        conditions,
                    },
                );
            }
        }
    }
    for mode in MODES {
        let firmware = if mode == "jit" {
            "exec-bench-aot".into()
        } else {
            format!("exec-bench-{mode}")
        };
        add(
            firmware,
            Cell {
                experiment: "exec_bench",
                mode,
                variant: "metrics",
                conditions: json!({ "programs": EXEC, "batches_per_program": 2 }),
            },
        );
    }
    add(
        "case-xdp".into(),
        Cell {
            experiment: "case",
            mode: "aot",
            variant: "trace",
            conditions: json!({ "direction": "A→B", "switches": 1 }),
        },
    );
    groups
}

pub fn preview() -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&plan())?);
    Ok(())
}

pub async fn run(cfg: &FileConfig) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("medium needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    agent.health().await.context("medium Agent health")?;
    agent.medium_preflight().await?;
    let cells = plan().iter().map(|group| group.cells.len()).sum::<usize>();
    anyhow::bail!(
        "{cells}-cell medium plan checked. Network Metrics/Trace and EXB2 Metrics collectors, embeprom, and all-mode database reconciliation remain pending"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    #[test]
    fn current_medium_cells_are_unique() {
        let groups = super::plan();
        let cells = groups
            .iter()
            .flat_map(|group| &group.cells)
            .collect::<Vec<_>>();
        let keys = cells
            .iter()
            .map(|cell| (cell.experiment, cell.mode, cell.variant))
            .collect::<BTreeSet<_>>();
        assert_eq!(cells.len(), 29);
        assert_eq!(keys.len(), 29);
        assert_eq!(
            keys.iter()
                .filter(|key| key.0 == "case")
                .copied()
                .collect::<Vec<_>>(),
            [("case", "aot", "trace")]
        );
        assert!(
            keys.iter()
                .filter(|key| key.0 == "exec_bench")
                .all(|key| key.2 == "metrics")
        );
    }
}
