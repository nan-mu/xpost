//! Fixed medium plan. Keep rates and repeats inside each firmware group.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    agent::{AgentClient, DeploymentReport},
    config::FileConfig,
    db::protocol,
};

const MODES: [&str; 4] = ["hal", "aot", "jit", "rbpf"];
const NETWORK: [&str; 3] = ["firewall", "just_drop", "just_tx"];
#[derive(Serialize)]
struct Group {
    firmware: String,
    cells: Vec<Cell>,
}

#[derive(Serialize)]
struct FirmwareGroup {
    firmware_sha256: String,
    profiles: Vec<Group>,
}

#[derive(Serialize)]
struct Readiness {
    release: String,
    groups: Vec<FirmwareGroup>,
    unavailable: Vec<String>,
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
                conditions: super::exb2::conditions(),
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
    let ready = preflight(cfg).await?;
    println!("{}", serde_json::to_string(&ready)?);
    ensure!(
        ready.unavailable.is_empty(),
        "medium collectors unavailable: {}",
        ready.unavailable.join(", ")
    );
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("medium needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    let mut database = protocol::connect(&cfg.postgres).await?;
    let expected = expected_attempts(&plan());
    let (resume, mut order) = match cfg.medium_resume_id {
        Some(id) => Resume::load(&database, id).await?,
        None => (Resume::default(), 0),
    };
    let medium_id = resume.id;
    let start = std::time::Instant::now();
    let mut actions = Actions::default();
    let mut problems = Vec::new();
    println!(
        "XPOST_MEDIUM_START {}",
        json!({
            "medium_id": medium_id, "planned_attempts": expected.len(),
            "previously_complete": resume.completed.len(),
        })
    );
    for group in ready.groups {
        if let Err(error) = run_group(
            cfg,
            &agent,
            &mut database,
            group,
            &resume,
            &mut order,
            &mut actions,
        )
        .await
        {
            eprintln!("medium group stopped: {error:#}");
            problems.push(format!("{error:#}"));
            match protocol::connect(&cfg.postgres).await {
                Ok(recovered) => database = recovered,
                Err(error) => {
                    problems.push(format!("reconnect protocol database: {error:#}"));
                    break;
                }
            }
        }
    }
    let audit = reconcile(&database, medium_id, &expected).await?;
    let elapsed_s = start.elapsed().as_secs_f64();
    println!(
        "XPOST_MEDIUM_SUMMARY {}",
        json!({
            "medium_id": medium_id, "elapsed_s": elapsed_s, "actions": actions,
            "audit": audit, "problems": problems,
        })
    );
    ensure!(
        audit.missing.is_empty()
            && audit.duplicate.is_empty()
            && audit.extra.is_empty()
            && problems.is_empty()
            && elapsed_s <= 3600.0,
        "medium {medium_id} did not meet all attempt, duplication, and elapsed-time criteria"
    );
    Ok(())
}

#[derive(Default, Serialize)]
pub(super) struct Actions {
    pub firmware_flash_confirmed: u32,
    pub cold_install_confirmed: u32,
    pub sram_preload_confirmed: u32,
    pub preparation_publish_confirmed: u32,
    pub measured_publish_confirmed: u32,
}

impl Actions {
    pub(super) fn note_flash(&mut self, evidence: &Value) {
        self.firmware_flash_confirmed += 1;
        if let Some(images) = evidence["containers"].as_array() {
            self.cold_install_confirmed += images.len() as u32;
        }
    }
}

pub(super) struct Resume {
    id: Uuid,
    completed: BTreeSet<String>,
    prior_attempts: BTreeMap<String, Uuid>,
    case_requires_recovery: Option<Uuid>,
}

impl Default for Resume {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            completed: BTreeSet::new(),
            prior_attempts: BTreeMap::new(),
            case_requires_recovery: None,
        }
    }
}

impl Resume {
    async fn load(db: &Client, id: Uuid) -> Result<(Self, i32)> {
        let rows = db
            .query(
                "SELECT r.experiment,r.mode,r.variant,r.repeat_no,r.status,c.parameters,
                r.run_id,r.plan_order, EXISTS(SELECT 1 FROM bench.run_artifact ra
                    WHERE ra.run_id=r.run_id AND ra.role='case_publish_intent')
            FROM bench.run r JOIN bench.condition c USING(condition_id)
            WHERE r.phase='formal' AND c.parameters->>'medium_id'=$1 ORDER BY r.plan_order",
                &[&id.to_string()],
            )
            .await?;
        ensure!(
            !rows.is_empty(),
            "medium id {id} has no original attempt rows"
        );
        let mut resume = Self {
            id,
            ..Self::default()
        };
        let mut next_order = 0;
        for row in rows {
            let experiment: &str = row.get(0);
            let mode: &str = row.get(1);
            let variant: &str = row.get(2);
            let repeat: i32 = row.get(3);
            let status: &str = row.get(4);
            let parameters: Value = row.get(5);
            let order: i32 = row.get(7);
            next_order = next_order.max(
                order
                    .checked_add(1)
                    .context("medium attempt order exhausted")?,
            );
            let condition = attempt_condition(experiment, variant, &parameters);
            let key = attempt_key(experiment, mode, variant, &condition, repeat);
            let run_id = row.get::<_, Uuid>(6);
            if status == "complete" {
                resume.completed.insert(key);
            } else {
                resume.prior_attempts.insert(key, run_id);
                if experiment == "case" && (status == "running" || row.get::<_, bool>(8)) {
                    resume.case_requires_recovery = Some(run_id);
                }
            }
        }
        Ok((resume, next_order))
    }

    pub(super) fn should_run(
        &self,
        experiment: &str,
        mode: &str,
        variant: &str,
        condition: &str,
        repeat: i32,
    ) -> bool {
        !self
            .completed
            .contains(&attempt_key(experiment, mode, variant, condition, repeat))
    }

    pub(super) fn retry_of(
        &self,
        experiment: &str,
        mode: &str,
        variant: &str,
        condition: &str,
        repeat: i32,
    ) -> Option<Uuid> {
        self.prior_attempts
            .get(&attempt_key(experiment, mode, variant, condition, repeat))
            .copied()
    }

    fn cell_complete(&self, cell: &Cell) -> bool {
        cell_conditions(cell).into_iter().all(|(condition, count)| {
            (1..=count).all(|repeat| {
                !self.should_run(cell.experiment, cell.mode, cell.variant, &condition, repeat)
            })
        })
    }
}

async fn run_group(
    cfg: &FileConfig,
    agent: &AgentClient,
    db: &mut Client,
    group: FirmwareGroup,
    resume: &Resume,
    order: &mut i32,
    actions: &mut Actions,
) -> Result<()> {
    let hash = &group.firmware_sha256;
    let medium_id = resume.id;
    let mut flashed = false;
    let mut active = None;
    for profile in group.profiles {
        let firmware = &profile.firmware;
        for cell in profile.cells {
            if resume.cell_complete(&cell) {
                continue;
            }
            if cell.experiment == "case" {
                if let Some(run) = resume.case_requires_recovery {
                    bail!(
                        "case run_id {run} has a pending or possibly completed publish. Recover the original evidence before any new switch"
                    );
                }
                super::case::run_case(
                    cfg,
                    hash,
                    medium_id,
                    resume.retry_of("case", "aot", "trace", "A-to-B", 1),
                    *order,
                    actions,
                )
                .await?;
                *order += 1;
                continue;
            }
            if !flashed {
                let evidence = agent.flash_fw_evidence(firmware, Some(hash)).await?;
                actions.note_flash(&evidence);
                active = evidence["containers"]
                    .as_array()
                    .and_then(|containers| containers.first())
                    .and_then(|container| container["profile"].as_str())
                    .map(str::to_owned);
                agent.get_init().await?;
                flashed = true;
            }
            if cell.experiment == "exec_bench" {
                super::exec_bench::run_mode(
                    agent,
                    db,
                    &cfg.postgres,
                    super::exec_bench::Mode::parse(cell.mode)?,
                    hash,
                    Some(medium_id),
                    order,
                    Some((resume, &mut active, actions)),
                )
                .await
                .with_context(|| format!("exec_bench/{}/{}", cell.mode, cell.variant))?;
                continue;
            }
            if cell.mode != "hal" {
                let name = if cell.mode == "aot" {
                    cell.experiment.to_owned()
                } else {
                    format!("{}_{}", cell.experiment, cell.mode)
                };
                if active.as_deref() != Some(&name) {
                    agent.bind_xdp(&name, firmware, hash).await?;
                    actions.cold_install_confirmed += 1;
                    agent.get_init().await?;
                    active = Some(name);
                }
            }
            super::medium_network::run_cell(
                cfg,
                agent,
                db,
                cell.experiment,
                cell.mode,
                cell.variant,
                firmware,
                hash,
                medium_id,
                order,
                resume,
            )
            .await
            .with_context(|| format!("{}/{}/{}", cell.experiment, cell.mode, cell.variant))?;
        }
    }
    Ok(())
}

fn expected_attempts(groups: &[Group]) -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    for cell in groups.iter().flat_map(|group| &group.cells) {
        for (condition, repeats) in cell_conditions(cell) {
            for repeat in 1..=repeats {
                expected.insert(attempt_key(
                    cell.experiment,
                    cell.mode,
                    cell.variant,
                    &condition,
                    repeat,
                ));
            }
        }
    }
    expected
}

fn cell_conditions(cell: &Cell) -> Vec<(String, i32)> {
    match (cell.experiment, cell.variant) {
        ("exec_bench", _) => super::exb2::PROGRAMS
            .into_iter()
            .map(|name| (name.to_owned(), 2))
            .collect::<Vec<_>>(),
        ("case", _) => vec![("A-to-B".into(), 1)],
        (_, "metrics") => [400, 1400, 4000]
            .into_iter()
            .map(|pps| (pps.to_string(), 2))
            .collect(),
        ("firewall", "trace") => vec![("hit".into(), 1), ("miss".into(), 1)],
        _ => vec![("tagged".into(), 1)],
    }
}

fn attempt_condition(experiment: &str, variant: &str, parameters: &Value) -> String {
    match (experiment, variant) {
        ("case", _) => "A-to-B".into(),
        ("exec_bench", _) => parameters["workload"].as_str().unwrap_or("").into(),
        (_, "metrics") => parameters["pps"].to_string(),
        _ => parameters["probe"].as_str().unwrap_or("").into(),
    }
}

fn attempt_key(experiment: &str, mode: &str, variant: &str, input: &str, repeat: i32) -> String {
    format!("{experiment}/{mode}/{variant}/{input}/{repeat}")
}

#[derive(Serialize)]
struct Reconciliation {
    planned: usize,
    completed: usize,
    missing: Vec<String>,
    duplicate: Vec<String>,
    extra: Vec<String>,
    status_counts: BTreeMap<String, usize>,
}

async fn reconcile(
    db: &Client,
    medium_id: Uuid,
    expected: &BTreeSet<String>,
) -> Result<Reconciliation> {
    let rows = db
        .query(
            "SELECT r.experiment,r.mode,r.variant,r.repeat_no,r.status,c.parameters
        FROM bench.run r JOIN bench.condition c USING(condition_id)
        WHERE r.phase='formal' AND c.parameters->>'medium_id'=$1",
            &[&medium_id.to_string()],
        )
        .await?;
    let mut status_counts = BTreeMap::new();
    let mut observed = BTreeSet::new();
    let mut duplicate = Vec::new();
    let mut extra = Vec::new();
    for row in rows {
        let (experiment, mode, variant, status): (&str, &str, &str, &str) =
            (row.get(0), row.get(1), row.get(2), row.get(4));
        let parameters: Value = row.get(5);
        let field = attempt_condition(experiment, variant, &parameters);
        let key = attempt_key(experiment, mode, variant, &field, row.get(3));
        *status_counts.entry(status.to_owned()).or_insert(0) += 1;
        if !expected.contains(&key) {
            extra.push(key);
        } else if status == "complete" && !observed.insert(key.clone()) {
            duplicate.push(key);
        }
    }
    let missing = expected.difference(&observed).cloned().collect();
    Ok(Reconciliation {
        planned: expected.len(),
        completed: observed.len(),
        missing,
        duplicate,
        extra,
        status_counts,
    })
}

pub async fn check(cfg: &FileConfig) -> Result<()> {
    let ready = preflight(cfg).await?;
    println!("{}", serde_json::to_string_pretty(&ready)?);
    ensure!(
        ready.unavailable.is_empty(),
        "medium collectors unavailable: {}",
        ready.unavailable.join(", ")
    );
    Ok(())
}

async fn preflight(cfg: &FileConfig) -> Result<Readiness> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("medium needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    tokio::time::timeout(Duration::from_secs(15), agent.health())
        .await
        .context("medium Agent health deadline")?
        .context("medium Agent health")?;
    let deployments = tokio::time::timeout(Duration::from_secs(180), agent.medium_preflight())
        .await
        .context("medium deployment preflight deadline")??;
    ensure!(
        deployments.release == "v0.1.1",
        "medium requires the v0.1.1 release"
    );
    let planned = plan();
    let tables = protocol_tables(&planned);
    let mut unavailable = BTreeSet::new();
    for cell in planned.iter().flat_map(|group| &group.cells) {
        let supported = match (cell.experiment, cell.mode, cell.variant) {
            ("case", "aot", "trace") => true,
            ("exec_bench", mode, "metrics") => MODES.contains(&mode),
            (experiment, mode, "metrics" | "trace") => {
                NETWORK.contains(&experiment) && MODES.contains(&mode)
            }
            _ => false,
        };
        if !supported {
            unavailable.insert(format!(
                "{}/{}/{}",
                cell.experiment, cell.mode, cell.variant
            ));
        }
    }
    let groups = bind_firmware(planned, &deployments)?;
    eprintln!(
        "medium Agent health and deployment identities verified for {} Firmware ELF digests",
        groups.len()
    );
    let mut database = protocol::connect(&cfg.postgres).await?;
    protocol::check(&mut database, &tables).await?;
    check_host(cfg)?;
    if let Some(id) = cfg.medium_resume_id {
        Resume::load(&database, id).await?;
    }
    Ok(Readiness {
        release: deployments.release,
        groups,
        unavailable: unavailable.into_iter().collect(),
    })
}

fn check_host(cfg: &FileConfig) -> Result<()> {
    use crate::pipeline::{RawTx, iface_ipv4, iface_mac};

    let iface = &cfg.probe.iface;
    let src = iface_ipv4(iface)?;
    let mac = match cfg.src_mac_override()? {
        Some(mac) => mac,
        None => iface_mac(iface)?,
    };
    let dut_mac = cfg.esp_mac_bytes()?;
    let target = &cfg.target;
    ensure!(
        src != target.esp_ip && mac != dut_mac,
        "host and DUT must have separate identities"
    );
    ensure!(
        target.echo_port == 8080 && target.stim_sport != 0 && target.stim_sport != 8080,
        "medium requires DUT UDP 8080 and a distinct source port"
    );
    RawTx::open(
        iface,
        mac,
        dut_mac,
        target.esp_ip,
        target.echo_port,
        target.stim_sport,
    )?;
    let capture = std::process::Command::new("tcpdump")
        .args(["-J", "-i", iface])
        .output()
        .context("probe tcpdump timestamp support")?;
    let message = String::from_utf8_lossy(&capture.stderr);
    let stdout = String::from_utf8_lossy(&capture.stdout);
    ensure!(
        capture.status.success()
            && message
                .lines()
                .chain(stdout.lines())
                .any(|line| line.trim_start().starts_with("host (")),
        "tcpdump on {iface} lacks host timestamps: {message}"
    );
    eprintln!("medium raw socket and tcpdump host timestamps verified on {iface}");
    Ok(())
}

fn protocol_tables(plan: &[Group]) -> Vec<String> {
    let mut tables = [
        "condition",
        "run",
        "artifact",
        "run_artifact",
        "clock",
        "calibration",
        "calibration_sample",
        "event_definition",
        "send_attempt",
        "capture",
        "packet_fact",
        "analysis",
    ]
    .map(|name| format!("bench.{name}"))
    .into_iter()
    .collect::<BTreeSet<_>>();
    for cell in plan.iter().flat_map(|group| &group.cells) {
        let (mode, experiment, variant) = (cell.mode, cell.experiment, cell.variant);
        tables.insert(format!("{mode}.{experiment}_config"));
        tables.insert(format!("{mode}.{experiment}_{variant}_data"));
        if variant == "trace" {
            tables.insert(format!("{mode}.{experiment}_event"));
        } else {
            for table in [
                "metric_definition",
                "metric_series",
                "metric_snapshot",
                "scalar_sample",
                "histogram_sample",
                "histogram_bucket",
            ] {
                tables.insert(format!("{mode}.{table}"));
            }
        }
        let records = match experiment {
            "firewall" => &["firewall_token"][..],
            "exec_bench" => &["exec_batch"],
            "case" => &["case_switch"],
            _ => &["probe_result"],
        };
        tables.extend(records.iter().map(|table| format!("{mode}.{table}")));
    }
    tables.into_iter().collect()
}

fn bind_firmware(plan: Vec<Group>, report: &DeploymentReport) -> Result<Vec<FirmwareGroup>> {
    let mut groups: Vec<FirmwareGroup> = Vec::new();
    for profile in plan {
        let mut hashes = BTreeSet::new();
        for cell in &profile.cells {
            if cell.mode == "hal" {
                let identity = report
                    .hal_firmware
                    .iter()
                    .find(|fw| fw.firmware_profile == profile.firmware)
                    .with_context(|| {
                        format!("missing HAL Firmware ELF for {}", profile.firmware)
                    })?;
                hashes.insert(identity.firmware_sha256.as_str());
                continue;
            }
            let programs: &[&str] = match cell.experiment {
                "case" => &["case_swap", "case_mark"],
                "exec_bench" => &super::exb2::PROGRAMS,
                _ => std::slice::from_ref(&cell.experiment),
            };
            for &program in programs {
                let slot = u8::from(program == "case_mark");
                let xdp = match (program, slot, cell.mode) {
                    ("case_mark", 1, "aot") => "case_mark_idle".to_owned(),
                    (_, _, "aot") => program.to_owned(),
                    (_, _, mode) => format!("{program}_{mode}"),
                };
                let identity = report
                    .prepared
                    .iter()
                    .find(|image| {
                        image.program == program
                            && image.mode == cell.mode
                            && image.profile == xdp
                            && image.slot == slot
                            && image.firmware.firmware_profile == profile.firmware
                    })
                    .with_context(|| {
                        format!(
                            "missing preflight target {} / {xdp} / slot {slot}",
                            profile.firmware
                        )
                    })?;
                hashes.insert(identity.firmware.firmware_sha256.as_str());
            }
        }
        ensure!(
            hashes.len() == 1,
            "inconsistent Firmware ELF for {}",
            profile.firmware
        );
        let hash = hashes.first().copied().expect("one firmware digest");
        ensure!(
            hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid Firmware ELF digest for {}",
            profile.firmware
        );
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.firmware_sha256 == hash)
        {
            group.profiles.push(profile);
        } else {
            groups.push(FirmwareGroup {
                firmware_sha256: hash.to_owned(),
                profiles: vec![profile],
            });
        }
    }
    Ok(groups)
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
        assert_eq!(super::expected_attempts(&groups).len(), 145);
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

    #[test]
    fn resume_selects_the_missing_condition_and_repeat() {
        let mut resume = super::Resume::default();
        let key = super::attempt_key("firewall", "hal", "metrics", "400", 1);
        let original = super::Uuid::new_v4();
        resume.prior_attempts.insert(key.clone(), original);
        assert_eq!(
            resume.retry_of("firewall", "hal", "metrics", "400", 1),
            Some(original)
        );
        assert!(resume.should_run("firewall", "hal", "metrics", "400", 1));
        resume.completed.insert(key);
        assert!(!resume.should_run("firewall", "hal", "metrics", "400", 1));
        assert!(resume.should_run("firewall", "hal", "metrics", "400", 2));
        assert!(resume.should_run("firewall", "jit", "metrics", "400", 1));
    }

    #[tokio::test]
    #[ignore = "requires the dedicated schema in a disposable XPOST_CASE_TEST_PG database"]
    async fn medium_protocol_preflight_is_read_only_and_reports_missing_tables()
    -> anyhow::Result<()> {
        let dsn = std::env::var("XPOST_CASE_TEST_PG")?;
        let (mut client, session) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls).await?;
        tokio::spawn(session);
        let tables = super::protocol_tables(&super::plan());
        super::protocol::check(&mut client, &tables).await?;
        let error = super::protocol::check(&mut client, &["aot.missing_fixture_table".into()])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("aot.missing_fixture_table"));
        Ok(())
    }
}
