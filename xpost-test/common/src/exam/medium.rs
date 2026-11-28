//! Run the selected medium phase in experiment order.
//! Group each experiment by Firmware ELF digest.
//! Keep rates and repeats inside each firmware group.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

use super::{exb2::Sampling, exec_bench::Mode as ExecMode, medium_network::NetworkPlan};
use crate::{
    agent::{AgentClient, DeploymentReport},
    config::{FileConfig, MediumStage},
    db::{Collection, Database},
};

const MODES: [&str; 4] = ["hal", "aot", "jit", "rbpf"];
const NETWORK: [&str; 3] = ["firewall", "just_tx", "just_drop"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Volume {
    Medium,
    All,
}

impl Volume {
    fn network(self) -> NetworkPlan {
        match self {
            Self::Medium => NetworkPlan::MEDIUM,
            Self::All => NetworkPlan::FULL,
        }
    }

    fn sampling(self) -> Sampling {
        match self {
            Self::Medium => Sampling::MEDIUM,
            Self::All => Sampling::FULL,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::All => "all",
        }
    }

    fn accepts_duration(self, elapsed: Duration) -> bool {
        self == Self::All || elapsed <= Duration::from_secs(3600)
    }
}
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
    volume: &'static str,
    stage: &'static str,
    groups: Vec<FirmwareGroup>,
    unavailable: Vec<String>,
    #[serde(skip)]
    identities: BTreeMap<String, Value>,
    #[serde(skip)]
    deployments: BTreeMap<(String, String), String>,
}

#[derive(Serialize)]
struct Cell {
    experiment: &'static str,
    mode: &'static str,
    variant: &'static str,
    conditions: Value,
    #[serde(skip)]
    exec_programs: &'static [&'static str],
}

fn plan(volume: Volume, stage: MediumStage) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let network = volume.network();
    let mut add = |firmware, cell: Cell| {
        if let Some(group) = groups.iter_mut().find(|group| {
            group.firmware == firmware && group.cells[0].experiment == cell.experiment
        }) {
            group.cells.push(cell);
        } else {
            groups.push(Group {
                firmware,
                cells: vec![cell],
            });
        }
    };
    for mode in stage.modes() {
        let exec_mode = ExecMode::parse(mode).expect("medium EXB2 mode");
        let programs = exec_mode.programs();
        let mut add_exec = |programs: &'static [&'static str]| {
            add(
                exec_mode.firmware(programs[0]),
                Cell {
                    experiment: "exec_bench",
                    mode,
                    variant: "metrics",
                    conditions: super::exb2::conditions(volume.sampling(), programs),
                    exec_programs: programs,
                },
            );
        };
        if exec_mode == ExecMode::Hal {
            for program in programs {
                add_exec(std::slice::from_ref(program));
            }
        } else {
            add_exec(programs);
        }
    }
    for experiment in NETWORK {
        let family = experiment.replace('_', "-");
        for mode in stage.modes() {
            for variant in ["metrics", "trace"] {
                let firmware = match (mode, variant) {
                    ("hal", _) => format!("{family}-hal"),
                    ("rbpf", "metrics") => format!("{family}-rbpf"),
                    ("rbpf", "trace") => format!("{family}-rbpf-trace"),
                    _ => format!("{family}-{variant}"),
                };
                let conditions = if variant == "metrics" {
                    json!({ "pps": network.rates, "window_s": network.window.as_secs(),
                        "observation_ms": network.observation.as_millis() as u64, "repeats": network.repeats })
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
                        exec_programs: &[],
                    },
                );
            }
        }
    }
    if volume == Volume::All && stage == MediumStage::JitRbpf {
        add(
            "case-xdp".into(),
            Cell {
                experiment: "case",
                mode: "aot",
                variant: "trace",
                conditions: json!({"probes": ["A-to-B"]}),
                exec_programs: &[],
            },
        );
    }

    groups
}

pub fn preview(stage: MediumStage, volume: Volume) -> Result<()> {
    let phase = |stage: MediumStage| {
        let groups = plan(volume, stage);
        let cells = groups.iter().flat_map(|group| &group.cells);
        let count = cells
            .clone()
            .map(|cell| (cell.experiment, cell.mode, cell.variant))
            .collect::<BTreeSet<_>>()
            .len();
        let attempts = cells
            .flat_map(cell_conditions)
            .map(|(_, repeats)| repeats)
            .sum::<i32>();
        json!({"stage": stage.as_str(), "cells": count, "planned_attempts": attempts, "groups": groups})
    };
    let output = match volume {
        Volume::Medium => phase(stage),
        Volume::All => json!({"volume": volume.label(), "selected_stage": stage.as_str(),
            "stages": [phase(MediumStage::HalAot), phase(MediumStage::JitRbpf)]}),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

pub(super) fn revision() -> Option<&'static str> {
    option_env!("XPOST_MEDIUM_REVISION")
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub async fn run(cfg: &FileConfig, volume: Volume) -> Result<()> {
    let revision = revision().context("build the formal runner with xpost-test.fish")?;
    let preflight_started = Instant::now();
    ensure!(
        cfg.medium_stage == MediumStage::HalAot || cfg.medium_resume_id.is_some(),
        "JIT/rBPF requires the HAL/AOT campaign ID"
    );
    let (ready, deployments) = preflight(cfg, volume).await?;
    println!("{}", serde_json::to_string(&ready)?);
    ensure!(
        ready.unavailable.is_empty(),
        "medium collectors unavailable: {}",
        ready.unavailable.join(", ")
    );
    let settings = Database::load()?;
    let collection = if cfg.medium_resume_id.is_some() {
        settings.resume().await?
    } else {
        settings.prepare().await?
    };
    collection
        .protect(async {
            check_resume(cfg, volume, &ready, &deployments, collection.client()).await?;
            collect(cfg, volume, ready, &collection, revision, preflight_started).await
        })
        .await
}

async fn collect(
    cfg: &FileConfig,
    volume: Volume,
    ready: Readiness,
    collection: &Collection<'_>,
    revision: &str,
    preflight_started: Instant,
) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("medium needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    let mut database = collection.connect().await?;
    let expected: BTreeSet<String> = ready.identities.keys().cloned().collect();
    let medium_id = cfg.medium_resume_id.unwrap_or_else(Uuid::new_v4);
    collection.lock_campaign(medium_id).await?;
    let (mut resume, mut order) = match cfg.medium_resume_id {
        Some(id) => Resume::load(&database, id, &ready.identities).await?,
        None => (
            Resume {
                id: medium_id,
                ..Resume::default()
            },
            0,
        ),
    };
    resume.identities = ready.identities;
    let preflight_s = preflight_started.elapsed().as_secs_f64();
    let start = Instant::now();
    let mut actions = Actions::default();
    let mut problems = Vec::new();
    let mut device = DeviceDeployment {
        attest_on_resume: cfg.medium_resume_id.is_some(),
        ..DeviceDeployment::default()
    };
    println!(
        "XPOST_MEDIUM_START {}",
        json!({
            "medium_id": medium_id, "stage": cfg.medium_stage.as_str(), "volume": volume.label(),
            "revision": revision, "planned_attempts": expected.len(), "preflight_s": preflight_s,
            "previously_complete": resume.completed.len(), "incompatible": resume.incompatible,
        })
    );
    for (index, group) in ready.groups.iter().enumerate() {
        let experiment = group.profiles[0].cells[0].experiment;
        if cfg
            .medium_resume_experiment
            .as_deref()
            .is_some_and(|selected| selected != experiment)
        {
            continue;
        }
        if let Err(error) = run_group(
            cfg,
            volume,
            &agent,
            collection,
            &mut database,
            group,
            &mut device,
            &ready.deployments,
            &resume,
            &mut order,
            &mut actions,
        )
        .await
        {
            device = DeviceDeployment::default();
            println!(
                "XPOST_GROUP_BLOCKED {}",
                json!({
                    "medium_id": medium_id, "stage": cfg.medium_stage.as_str(),
                    "group": group, "error": format!("{error:#}"),
                })
            );
            problems.push(format!("{error:#}"));
            match collection.connect().await {
                Ok(recovered) => database = recovered,
                Err(error) => {
                    problems.push(format!("reconnect protocol database: {error:#}"));
                    break;
                }
            }
        }
        if ready
            .groups
            .get(index + 1)
            .is_none_or(|next| next.profiles[0].cells[0].experiment != experiment)
        {
            let audit = reconcile(&database, medium_id, &expected, &resume.identities).await?;
            println!(
                "XPOST_MEDIUM_CHECKPOINT {}",
                json!({
                    "medium_id": medium_id, "stage": cfg.medium_stage.as_str(),
                    "experiment": experiment, "audit": audit,
                })
            );
        }
    }
    let audit = reconcile(&database, medium_id, &expected, &resume.identities).await?;
    let elapsed = start.elapsed();
    println!(
        "XPOST_MEDIUM_SUMMARY {}",
        json!({
            "medium_id": medium_id, "stage": cfg.medium_stage.as_str(), "volume": volume.label(),
            "elapsed_s": elapsed.as_secs_f64(), "actions": actions,
            "audit": audit, "problems": problems,
        })
    );
    if volume == Volume::All && cfg.medium_stage == MediumStage::HalAot {
        check_first_stage(&audit)?;
    }
    let covers_plan = match volume {
        Volume::Medium => audit.missing.is_empty(),
        Volume::All => audit.unrecorded.is_empty(),
    };
    ensure!(
        covers_plan
            && audit.duplicate.is_empty()
            && audit.extra.is_empty()
            && problems.is_empty()
            && volume.accepts_duration(elapsed),
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
    pub firmware_flash_s: f64,
    pub cold_install_s: f64,
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
    identities: BTreeMap<String, Value>,
    prior_attempts: BTreeMap<String, Uuid>,
    case_requires_recovery: Option<Uuid>,
    incompatible: Vec<Incompatible>,
}

impl Default for Resume {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            completed: BTreeSet::new(),
            identities: BTreeMap::new(),
            prior_attempts: BTreeMap::new(),
            case_requires_recovery: None,
            incompatible: Vec::new(),
        }
    }
}

impl Resume {
    async fn load(
        db: &Client,
        id: Uuid,
        identities: &BTreeMap<String, Value>,
    ) -> Result<(Self, i32)> {
        let rows = attempt_rows(db, id).await?;
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
            if qualifies(identities, &key, status, &parameters, row.get(8)) {
                resume.completed.insert(key);
            } else {
                if identities.contains_key(&key) && !compatible(identities, &key, &parameters) {
                    resume.incompatible.push(Incompatible {
                        key: key.clone(),
                        run_id,
                    });
                }
                resume.prior_attempts.insert(key, run_id);
                if experiment == "case"
                    && !resume.completed.contains("case/aot/trace/A-to-B/1")
                    && (status == "complete" || row.get::<_, bool>(9))
                {
                    resume.case_requires_recovery = Some(run_id);
                }
            }
        }
        Ok((resume, next_order))
    }

    pub(super) fn identity(
        &self,
        experiment: &str,
        mode: &str,
        variant: &str,
        condition: &str,
        repeat: i32,
    ) -> Option<&Value> {
        self.identities
            .get(&attempt_key(experiment, mode, variant, condition, repeat))
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

#[derive(Default)]
struct DeviceDeployment {
    firmware_sha256: Option<String>,
    active: Option<String>,
    attest_on_resume: bool,
}

async fn run_group(
    cfg: &FileConfig,
    volume: Volume,
    agent: &AgentClient,
    settings: &Collection<'_>,
    db: &mut Client,
    group: &FirmwareGroup,
    device: &mut DeviceDeployment,
    deployments: &BTreeMap<(String, String), String>,
    resume: &Resume,
    order: &mut i32,
    actions: &mut Actions,
) -> Result<()> {
    let hash = &group.firmware_sha256;
    let medium_id = resume.id;
    for profile in &group.profiles {
        let firmware = &profile.firmware;
        for cell in &profile.cells {
            if resume.cell_complete(cell) {
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
                    settings,
                    hash,
                    medium_id,
                    resume
                        .identity("case", "aot", "trace", "A-to-B", 1)
                        .context("missing case plan identity")?,
                    resume.retry_of("case", "aot", "trace", "A-to-B", 1),
                    *order,
                    actions,
                )
                .await?;
                *order += 1;
                *device = DeviceDeployment::default();
                continue;
            }
            if std::mem::take(&mut device.attest_on_resume) {
                let observed = agent.attest_deployment(firmware, hash).await?;
                if observed.firmware_matches {
                    agent.get_init().await?;
                    device.firmware_sha256 = Some(hash.clone());
                    device.active = observed
                        .container_sha256
                        .and_then(|digest| deployments.get(&(hash.clone(), digest)).cloned());
                }
            }
            if device.firmware_sha256.as_deref() != Some(hash) {
                let flash_started = Instant::now();
                let evidence = agent.flash_fw_evidence(firmware, Some(hash)).await?;
                actions.firmware_flash_s += flash_started.elapsed().as_secs_f64();
                actions.note_flash(&evidence);
                device.active = evidence["containers"]
                    .as_array()
                    .and_then(|containers| containers.first())
                    .and_then(|container| container["profile"].as_str())
                    .map(str::to_owned);
                agent.get_init().await?;
                device.firmware_sha256 = Some(hash.clone());
            }
            if cell.experiment == "exec_bench" {
                super::exec_bench::run_mode(
                    agent,
                    db,
                    settings,
                    ExecMode::parse(cell.mode)?,
                    cell.exec_programs,
                    hash,
                    Some(medium_id),
                    order,
                    volume.sampling(),
                    super::exec_bench::TrialPlan::Campaign(resume, &mut device.active, actions),
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
                if device.active.as_deref() != Some(&name) {
                    let install_started = Instant::now();
                    agent.bind_xdp(&name, firmware, hash).await?;
                    actions.cold_install_s += install_started.elapsed().as_secs_f64();
                    actions.cold_install_confirmed += 1;
                    agent.get_init().await?;
                    device.active = Some(name);
                }
            }
            super::medium_network::qualify_group(cfg, agent, cell.experiment, cell.mode).await?;
            super::medium_network::run_cell(
                cfg,
                agent,
                settings,
                db,
                cell.experiment,
                cell.mode,
                cell.variant,
                firmware,
                hash,
                medium_id,
                volume.network(),
                &cell_conditions(cell),
                order,
                resume,
            )
            .await
            .with_context(|| format!("{}/{}/{}", cell.experiment, cell.mode, cell.variant))?;
        }
    }
    Ok(())
}

fn planned_identities(
    cfg: &FileConfig,
    volume: Volume,
    stage: MediumStage,
    groups: &[FirmwareGroup],
    report: &DeploymentReport,
) -> Result<BTreeMap<String, Value>> {
    let mut identities = BTreeMap::new();
    for group in groups {
        for profile in &group.profiles {
            for cell in &profile.cells {
                for (condition, repeats) in cell_conditions(cell) {
                    let prepared_sha = |program: &str, slot: u8| -> Result<&str> {
                        let prepared = report
                            .prepared
                            .iter()
                            .find(|image| {
                                image.program == program
                                    && image.mode == cell.mode
                                    && image.slot == slot
                                    && image.firmware.firmware_profile == profile.firmware
                            })
                            .context("missing planned deployment identity")?;
                        ensure!(
                            prepared.container_sha256.len() == 64,
                            "missing prepared XDP ELF digest"
                        );
                        Ok(prepared.container_sha256.as_str())
                    };
                    let deployment = match (cell.experiment, cell.mode) {
                        (_, "hal") => None,
                        ("case", _) => Some(prepared_sha("case_swap", 0)?),
                        ("exec_bench", _) => Some(prepared_sha(&condition, 0)?),
                        (program, _) => Some(prepared_sha(program, 0)?),
                    };
                    let input = if cell.experiment == "exec_bench" {
                        cell.conditions["workloads"]
                            .as_array()
                            .context("missing EXB2 inputs")?
                            .iter()
                            .find(|input| input["program"] == condition)
                            .context("missing EXB2 workload input")?
                            .clone()
                    } else if cell.experiment == "case" {
                        super::case::planned_input(cfg, prepared_sha("case_mark", 1)?)?
                    } else {
                        super::medium_network::planned_input(
                            cfg,
                            cell.experiment,
                            cell.variant,
                            &condition,
                            volume.network(),
                        )?
                    };
                    let mut identity = json!({
                        "stage": stage.as_str(),
                        "firmware_sha256": group.firmware_sha256,
                        "container_sha256": deployment,
                        "input": input,
                    });
                    if volume == Volume::All {
                        identity["volume"] = json!(volume.label());
                    }
                    for repeat in 1..=repeats {
                        identities.insert(
                            attempt_key(
                                cell.experiment,
                                cell.mode,
                                cell.variant,
                                &condition,
                                repeat,
                            ),
                            identity.clone(),
                        );
                    }
                }
            }
        }
    }
    Ok(identities)
}

fn cell_conditions(cell: &Cell) -> Vec<(String, i32)> {
    if cell.experiment == "exec_bench" {
        let repeats = cell.conditions["runs_per_program"]
            .as_i64()
            .expect("EXB2 repeats") as i32;
        return cell
            .exec_programs
            .iter()
            .map(|&name| (name.to_owned(), repeats))
            .collect();
    }
    if cell.variant == "metrics" {
        let repeats = cell.conditions["repeats"]
            .as_i64()
            .expect("network repeats") as i32;
        return cell.conditions["pps"]
            .as_array()
            .expect("network rates")
            .iter()
            .map(|rate| (rate.to_string(), repeats))
            .collect();
    }
    cell.conditions["probes"]
        .as_array()
        .expect("Trace probes")
        .iter()
        .map(|probe| (probe.as_str().expect("named probe").to_owned(), 1))
        .collect()
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
    unrecorded: Vec<String>,
    duplicate: Vec<String>,
    extra: Vec<String>,
    incompatible: Vec<Incompatible>,
    status_counts: BTreeMap<String, usize>,
    accepted_run_ids: BTreeMap<String, Uuid>,
}

#[derive(Serialize)]
struct Incompatible {
    key: String,
    run_id: Uuid,
}

async fn reconcile(
    db: &Client,
    medium_id: Uuid,
    expected: &BTreeSet<String>,
    identities: &BTreeMap<String, Value>,
) -> Result<Reconciliation> {
    let rows = attempt_rows(db, medium_id).await?;
    let mut status_counts = BTreeMap::new();
    let mut observed = BTreeSet::new();
    let mut accepted_run_ids = BTreeMap::new();
    let mut duplicate = Vec::new();
    let mut extra = Vec::new();
    let mut incompatible = Vec::new();
    let mut terminal = BTreeSet::new();
    let stage = identities.values().next().context("empty campaign plan")?["stage"].clone();
    for row in rows {
        let (experiment, mode, variant, status): (&str, &str, &str, &str) =
            (row.get(0), row.get(1), row.get(2), row.get(4));
        let parameters: Value = row.get(5);
        let field = attempt_condition(experiment, variant, &parameters);
        let key = attempt_key(experiment, mode, variant, &field, row.get(3));
        if parameters["stage"] != stage {
            continue;
        }
        *status_counts.entry(status.to_owned()).or_insert(0) += 1;
        if !expected.contains(&key) {
            extra.push(key);
        } else if !compatible(identities, &key, &parameters) {
            incompatible.push(Incompatible {
                key,
                run_id: row.get(6),
            });
        } else {
            if terminal_attempt(status, &row.get(10)) {
                terminal.insert(key.clone());
            }
            if qualifies(identities, &key, status, &parameters, row.get(8)) {
                if !observed.insert(key.clone()) {
                    duplicate.push(key.clone());
                }
                accepted_run_ids.insert(key, row.get(6));
            }
        }
    }
    let missing = expected.difference(&observed).cloned().collect();
    Ok(Reconciliation {
        planned: expected.len(),
        completed: observed.len(),
        missing,
        unrecorded: expected.difference(&terminal).cloned().collect(),
        duplicate,
        extra,
        incompatible,
        status_counts,
        accepted_run_ids,
    })
}

/// An explicit replacement references an earlier analysis of the same stored evidence.
async fn attempt_rows(
    db: &impl tokio_postgres::GenericClient,
    campaign: Uuid,
) -> Result<Vec<tokio_postgres::Row>> {
    Ok(db.query(
        "SELECT r.experiment,r.mode,r.variant,r.repeat_no,r.status,c.parameters,r.run_id,r.plan_order,
        COALESCE((SELECT CASE WHEN count(*)=1 THEN bool_and(a.qualified AND
            (r.status <> 'inconclusive' OR EXISTS (
                SELECT 1 FROM release.bench_analysis previous
                WHERE previous.run_id=a.run_id AND previous.source_digest=a.source_digest
                  AND previous.created_at <= a.created_at
                  AND previous.analysis_id <> a.analysis_id
                  AND a.issues @> jsonb_build_array(jsonb_build_object('supersedes_analysis_id',previous.analysis_id))
            ))) ELSE false END
          FROM release.bench_analysis a WHERE a.run_id=r.run_id AND NOT EXISTS (
            SELECT 1 FROM release.bench_analysis newer
            WHERE newer.run_id=a.run_id AND newer.source_digest=a.source_digest
              AND newer.created_at >= a.created_at
              AND newer.analysis_id <> a.analysis_id
              AND newer.issues @> jsonb_build_array(jsonb_build_object('supersedes_analysis_id',a.analysis_id))
          )),false),
        EXISTS(SELECT 1 FROM release.bench_run_artifact ra WHERE ra.run_id=r.run_id AND ra.role='case_publish_intent'),r.issues
        FROM release.bench_run r JOIN release.bench_condition c USING(condition_id)
        WHERE r.phase='formal' AND c.parameters->>'medium_id'=$1
        ORDER BY r.plan_order",
        &[&campaign.to_string()],
    ).await?)
}

fn qualifies(
    identities: &BTreeMap<String, Value>,
    key: &str,
    status: &str,
    parameters: &Value,
    qualified: bool,
) -> bool {
    qualified
        && matches!(status, "complete" | "inconclusive")
        && compatible(identities, key, parameters)
}

fn compatible(identities: &BTreeMap<String, Value>, key: &str, parameters: &Value) -> bool {
    identities.get(key).is_some_and(|identity| {
        parameters["medium_identity"] == *identity && parameters["stage"] == identity["stage"]
    })
}

fn terminal_attempt(status: &str, issues: &Value) -> bool {
    status == "complete"
        || matches!(
            status,
            "failed" | "unsupported" | "inconclusive" | "blocked"
        ) && issues.as_array().is_some_and(|issues| !issues.is_empty())
}

pub async fn check(cfg: &FileConfig, volume: Volume) -> Result<()> {
    let (ready, deployments) = preflight(cfg, volume).await?;
    let settings = Database::load()?;
    let collection = settings.resume().await?;
    collection
        .protect(check_resume(
            cfg,
            volume,
            &ready,
            &deployments,
            collection.client(),
        ))
        .await?;
    println!("{}", serde_json::to_string_pretty(&ready)?);
    ensure!(
        ready.unavailable.is_empty(),
        "medium collectors unavailable: {}",
        ready.unavailable.join(", ")
    );
    Ok(())
}

async fn preflight(cfg: &FileConfig, volume: Volume) -> Result<(Readiness, DeploymentReport)> {
    let planned = plan(volume, cfg.medium_stage);
    if let Some(experiment) = &cfg.medium_resume_experiment {
        ensure!(
            cfg.medium_resume_id.is_some(),
            "medium_resume_experiment needs medium_resume_id"
        );
        ensure!(
            planned
                .iter()
                .flat_map(|group| &group.cells)
                .any(|cell| cell.experiment == experiment),
            "experiment {experiment} is outside the selected medium stage"
        );
    }
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("medium needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    tokio::time::timeout(Duration::from_secs(15), agent.health())
        .await
        .context("medium Agent health deadline")?
        .context("medium Agent health")?;
    let deployments = tokio::time::timeout(
        Duration::from_secs(180),
        agent.medium_preflight(Some(if volume == Volume::All {
            "full"
        } else {
            cfg.medium_stage.as_str()
        })),
    )
    .await
    .context("medium deployment preflight deadline")??;
    ensure!(
        deployments.release == "v0.1.1",
        "medium requires the v0.1.1 release"
    );
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
    let identities = planned_identities(cfg, volume, cfg.medium_stage, &groups, &deployments)?;
    let containers = deployments
        .prepared
        .iter()
        .map(|item| {
            (
                (
                    item.firmware.firmware_sha256.clone(),
                    item.container_sha256.clone(),
                ),
                item.profile.clone(),
            )
        })
        .collect();
    eprintln!(
        "medium Agent health and deployment identities verified for {} Firmware ELF digests",
        groups.len()
    );
    check_host(cfg)?;
    super::medium_network::check_capture_path(cfg)?;
    Ok((
        Readiness {
            release: deployments.release.clone(),
            volume: volume.label(),
            stage: cfg.medium_stage.as_str(),
            groups,
            unavailable: unavailable.into_iter().collect(),
            identities,
            deployments: containers,
        },
        deployments,
    ))
}

async fn check_resume(
    cfg: &FileConfig,
    volume: Volume,
    ready: &Readiness,
    deployments: &DeploymentReport,
    database: &Client,
) -> Result<()> {
    if let Some(id) = cfg.medium_resume_id {
        Resume::load(database, id, &ready.identities).await?;
        if cfg.medium_stage == MediumStage::JitRbpf {
            let first_deployments;
            let report = if volume == Volume::All {
                deployments
            } else {
                let endpoint = cfg
                    .agent
                    .resolved_endpoint()
                    .context("medium needs [agent].endpoint")?;
                first_deployments = AgentClient::new(endpoint)
                    .medium_preflight(Some("hal-aot"))
                    .await?;
                &first_deployments
            };
            let first = bind_firmware(plan(volume, MediumStage::HalAot), report)?;
            let first = planned_identities(cfg, volume, MediumStage::HalAot, &first, report)?;
            let expected = first.keys().cloned().collect();
            let audit = reconcile(database, id, &expected, &first).await?;
            check_first_stage(&audit)?;
        }
    } else {
        ensure!(
            cfg.medium_stage == MediumStage::HalAot,
            "JIT/rBPF needs the original HAL/AOT campaign ID"
        );
    }
    Ok(())
}

fn check_first_stage(audit: &Reconciliation) -> Result<()> {
    ensure!(
        audit.unrecorded.is_empty(),
        "HAL/AOT attempts lack compatible terminal records: {}",
        audit.unrecorded.join(", ")
    );
    for experiment in ["exec_bench", "firewall", "just_tx", "just_drop"] {
        for mode in MediumStage::HalAot.modes() {
            ensure!(
                audit
                    .accepted_run_ids
                    .keys()
                    .any(|key| key.starts_with(&format!("{experiment}/{mode}/metrics/"))),
                "missing plot-ready HAL/AOT Metrics for {experiment}/{mode}"
            );
        }
    }
    Ok(())
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
                "exec_bench" => cell.exec_programs,
                _ => std::slice::from_ref(&cell.experiment),
            };
            for &program in programs {
                let slot = u8::from(program == "case_mark");
                let identity = report
                    .prepared
                    .iter()
                    .find(|image| {
                        image.program == program
                            && image.mode == cell.mode
                            && image.slot == slot
                            && image.firmware.firmware_profile == profile.firmware
                    })
                    .with_context(|| {
                        format!(
                            "missing preflight target {} / {program} / {} / slot {slot}",
                            profile.firmware, cell.mode
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
        if let Some(group) = groups.iter_mut().find(|group| {
            group.firmware_sha256 == hash
                && group.profiles[0].cells[0].experiment == profile.cells[0].experiment
        }) {
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
    fn first_stage_plan_selects_only_hal_and_aot_in_experiment_order() {
        let groups = super::plan(super::Volume::Medium, super::MediumStage::HalAot);
        let cells = groups
            .iter()
            .flat_map(|group| &group.cells)
            .collect::<Vec<_>>();
        let keys = cells
            .iter()
            .map(|cell| (cell.experiment, cell.mode, cell.variant))
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), 14);
        assert_eq!(cells.len(), 21);
        assert_eq!(
            cells
                .iter()
                .flat_map(|cell| super::cell_conditions(cell))
                .map(|(_, repeats)| repeats)
                .sum::<i32>(),
            74
        );
        assert_eq!(
            keys.iter().map(|key| key.1).collect::<BTreeSet<_>>(),
            ["aot", "hal"].into()
        );
        let mut order = cells.iter().map(|cell| cell.experiment).collect::<Vec<_>>();
        order.dedup();
        assert_eq!(order, ["exec_bench", "firewall", "just_tx", "just_drop"]);
    }

    #[test]
    fn every_hal_workload_uses_a_distinct_profile_and_matmul_stays_hal_only() {
        for volume in [super::Volume::Medium, super::Volume::All] {
            let groups = super::plan(volume, super::MediumStage::HalAot);
            let hal = groups
                .iter()
                .filter(|group| {
                    group.cells[0].experiment == "exec_bench" && group.cells[0].mode == "hal"
                })
                .collect::<Vec<_>>();
            assert_eq!(hal.len(), 8);
            for (group, workload) in hal.into_iter().zip([
                "noop",
                "prime",
                "log2",
                "copy",
                "fletcher32",
                "bsort",
                "strcmp",
                "matmul",
            ]) {
                assert_eq!(group.firmware, format!("exec-bench-hal-{workload}"));
                assert_eq!(group.cells.len(), 1);
                let conditions = super::cell_conditions(&group.cells[0]);
                assert_eq!(conditions, [(workload.to_owned(), volume.sampling().runs)]);
            }
            for stage in [super::MediumStage::HalAot, super::MediumStage::JitRbpf] {
                for cell in super::plan(volume, stage)
                    .iter()
                    .flat_map(|group| &group.cells)
                {
                    if cell.mode != "hal" && cell.experiment == "exec_bench" {
                        let workloads = super::cell_conditions(cell);
                        assert_eq!(workloads.len(), 7);
                        assert!(workloads.iter().all(|(name, _)| name != "matmul"));
                    }
                }
            }
        }
    }

    #[test]
    fn resume_keys_bind_hal_matmul_to_its_own_digest_and_full_response() -> anyhow::Result<()> {
        use super::{MediumStage, Volume, bind_firmware, compatible, json, planned_identities};
        let volume = Volume::All;
        let stage = MediumStage::HalAot;
        let planned = super::plan(volume, stage)
            .into_iter()
            .filter(|group| group.firmware.starts_with("exec-bench-hal-"))
            .collect::<Vec<_>>();
        let firmware = planned
            .iter()
            .enumerate()
            .map(|(index, group)| {
                json!({
                    "firmware_profile": group.firmware,
                    "firmware_sha256": format!("{index:064x}")
                })
            })
            .collect::<Vec<_>>();
        let report = serde_json::from_value(json!({
            "release":"v0.1.1",
            "required":0,
            "deployments":[],
            "issues":[],
            "hal_firmware":firmware
        }))?;
        let groups = bind_firmware(planned, &report)?;
        let cfg = crate::config::FileConfig::embedded()?;
        let identities = planned_identities(&cfg, volume, stage, &groups, &report)?;
        let key = "exec_bench/hal/metrics/matmul/1";
        let matched = &identities[key];
        assert_eq!(matched["firmware_sha256"], firmware[7]["firmware_sha256"]);
        assert_eq!(matched["container_sha256"], serde_json::Value::Null);
        assert_eq!(matched["input"]["expected_result"], json!([19, 50]));
        assert_eq!(matched["input"]["scratch_bytes"], 16);
        assert_eq!(identities.len(), 40);
        let mut stored = json!({ "stage":"hal-aot", "medium_identity":matched });
        assert!(compatible(&identities, key, &stored));
        stored["medium_identity"]["firmware_sha256"] = firmware[0]["firmware_sha256"].clone();
        assert!(!compatible(&identities, key, &stored));

        // PR41 used the same Firmware ELF across every HAL EXB2 workload.
        let legacy_digest = "aa".repeat(32);
        for program in crate::exam::exb2::xdp_programs() {
            let key = format!("exec_bench/hal/metrics/{program}/1");
            let mut identity = identities[&key].clone();
            identity["firmware_sha256"] = json!(legacy_digest);
            let legacy = json!({ "stage":"hal-aot", "medium_identity":identity });
            assert!(!compatible(&identities, &key, &legacy), "{program}");
        }
        Ok(())
    }

    #[test]
    fn second_stage_excludes_hal_aot_and_keeps_the_same_order() {
        let groups = super::plan(super::Volume::Medium, super::MediumStage::JitRbpf);
        let cells = groups
            .iter()
            .flat_map(|group| &group.cells)
            .collect::<Vec<_>>();
        let modes = cells.iter().map(|cell| cell.mode).collect::<BTreeSet<_>>();
        assert_eq!(modes, ["jit", "rbpf"].into());
        assert_eq!(cells.len(), 14);
        assert_eq!(
            cells
                .iter()
                .flat_map(|cell| super::cell_conditions(cell))
                .map(|(_, repeats)| repeats)
                .sum::<i32>(),
            72
        );
        let mut order = cells.iter().map(|cell| cell.experiment).collect::<Vec<_>>();
        order.dedup();
        assert_eq!(order, ["exec_bench", "firewall", "just_tx", "just_drop"]);
    }

    #[test]
    fn all_has_630_attempts_with_hal_matmul() {
        use super::{MediumStage, Volume, cell_conditions, plan};
        let hal_aot = plan(Volume::All, MediumStage::HalAot);
        let jit_rbpf = plan(Volume::All, MediumStage::JitRbpf);
        let first = hal_aot.iter().flat_map(|group| &group.cells);
        let second = jit_rbpf.iter().flat_map(|group| &group.cells);
        assert_eq!(first.clone().count(), 21);
        assert_eq!(second.clone().count(), 15);
        assert_eq!(
            first.flat_map(cell_conditions).map(|(_, n)| n).sum::<i32>(),
            317
        );
        assert_eq!(
            second
                .flat_map(cell_conditions)
                .map(|(_, n)| n)
                .sum::<i32>(),
            313
        );
        let elapsed = std::time::Duration::from_secs(14_508);
        assert!(Volume::All.accepts_duration(elapsed));
        assert!(!Volume::Medium.accepts_duration(elapsed));
    }

    #[test]
    fn first_stage_needs_terminal_trace_and_all_eight_plot_pairs() {
        use super::{Reconciliation, Uuid, check_first_stage, json, terminal_attempt};
        assert!(!terminal_attempt(
            "running",
            &json!(["previous windows completed"])
        ));
        assert!(!terminal_attempt("inconclusive", &json!([])));
        assert!(terminal_attempt(
            "inconclusive",
            &json!(["capture incomplete"])
        ));
        let mut audit = Reconciliation {
            planned: 312,
            completed: 307,
            missing: vec![],
            unrecorded: vec![],
            duplicate: vec![],
            extra: vec![],
            incompatible: vec![],
            status_counts: Default::default(),
            accepted_run_ids: ["exec_bench", "firewall", "just_tx", "just_drop"]
                .into_iter()
                .flat_map(|experiment| {
                    ["hal", "aot"].map(move |mode| {
                        (
                            format!("{experiment}/{mode}/metrics/input/1"),
                            Uuid::new_v4(),
                        )
                    })
                })
                .collect(),
        };
        assert!(check_first_stage(&audit).is_ok());
        audit.unrecorded.push("firewall/hal/trace/miss/1".into());
        assert!(
            check_first_stage(&audit)
                .unwrap_err()
                .to_string()
                .contains("firewall/hal/trace/miss/1")
        );
        audit.unrecorded.clear();
        audit.accepted_run_ids.remove("just_tx/hal/metrics/input/1");
        assert!(
            check_first_stage(&audit)
                .unwrap_err()
                .to_string()
                .contains("just_tx/hal")
        );
    }

    #[test]
    fn capture_boundary_analysis_and_window_are_part_of_attempt_identity() {
        use super::{compatible, json};
        let key = "just_tx/hal/metrics/400/1";
        let planned = json!({"stage":"hal-aot","firmware_sha256":"same","volume":"all",
            "input":{"window_ns":30_000_000_000_u64,"capture":{"export_tail_ns":1_200_000_000_u64},
            "analysis_version":"campaign-network-v3"}});
        let identities = [(key.to_owned(), planned.clone())].into();
        let actual = json!({"stage":"hal-aot","medium_identity":planned});
        assert!(compatible(&identities, key, &actual));
        for (path, value) in [
            (
                "/medium_identity/input/capture/export_tail_ns",
                json!(1_100_000_000_u64),
            ),
            (
                "/medium_identity/input/analysis_version",
                json!("older-analysis"),
            ),
            ("/medium_identity/input/window_ns", json!(5_000_000_000_u64)),
        ] {
            let mut changed = actual.clone();
            *changed.pointer_mut(path).unwrap() = value;
            assert!(!compatible(&identities, key, &changed));
        }
    }

    #[tokio::test]
    #[ignore = "requires a disposable protocol database XPOST_CASE_TEST_DB_FILE"]
    async fn explicit_analysis_replacements_control_campaign_qualification() -> anyhow::Result<()> {
        use super::{Uuid, json};
        let database = crate::db::Database::load_fixture()?;
        let mut client = database.connect().await?;
        let tx = client.transaction().await?;
        let campaign = Uuid::new_v4();
        let identity = json!({"stage":"hal-aot","firmware_sha256":"fixture","input":{"pps":400}});
        let key = super::attempt_key("firewall", "hal", "metrics", "400", 1);
        let identities = [(key.clone(), identity.clone())].into();
        let parameters =
            json!({"medium_id":campaign,"stage":"hal-aot","medium_identity":identity,"pps":400});
        for scenario in 0..2 {
            let run = Uuid::new_v4();
            let condition = Uuid::new_v4();
            let config = Uuid::new_v4();
            let status = if scenario == 0 {
                "complete"
            } else {
                "inconclusive"
            };
            tx.execute("INSERT INTO release.bench_condition(condition_id,experiment,protocol_version,parameters,planned_modes)
                VALUES ($1,'firewall','1',$2,ARRAY['hal'])", &[&condition,&parameters]).await?;
            let fixture = Uuid::new_v4();
            let fixture_digest = "b".repeat(64);
            tx.execute("INSERT INTO release.bench_artifact(artifact_id,role,sha256,size_bytes,original_bytes,media_type)
                VALUES($1,'fixture',$2,0,''::bytea,'application/octet-stream')", &[&fixture,&fixture_digest]).await?;
            let profile_hash = "c".repeat(64);
            tx.execute("INSERT INTO release.hal_firewall_config(config_id,condition_id,instrumentation,profile_hash,target_pps,
                send_window_ns,drain_ns,payload_bytes,blacklist_capacity,blacklist_entries,legal_numerator,
                legal_denominator,lookup_case,map_fixture_artifact_id)
                VALUES($1,$2,'counter_only',$3,400,5000000000,250000000,42,256,1,1,10,'mixed',$4)",
                &[&config,&condition,&profile_hash,&fixture]).await?;
            tx.execute("INSERT INTO release.bench_run(run_id,condition_id,config_id,mode,experiment,variant,phase,repeat_no,seed,plan_order,status,run_uuid)
                VALUES($1,$2,$3,'hal','firewall','metrics','formal',1,0,$4,$5,$1)",
                &[&run,&condition,&config,&scenario,&status]).await?;
            let original = Uuid::new_v4();
            let replacement = Uuid::new_v4();
            let digest = "a".repeat(64);
            for (id, qualified, notes) in [
                (original, scenario == 0, json!([])),
                (
                    replacement,
                    scenario == 1,
                    json!([{"supersedes_analysis_id":original}]),
                ),
            ] {
                tx.execute("INSERT INTO release.bench_analysis(analysis_id,run_id,algorithm_version,source_digest,qualified,issues)
                    VALUES($1,$2,'fixture',$3,$4,$5)", &[&id,&run,&digest,&qualified,&notes]).await?;
            }
        }
        let rows = super::attempt_rows(&tx, campaign).await?;
        assert_eq!(rows.len(), 2);
        let accepted = rows
            .iter()
            .map(|row| super::qualifies(&identities, &key, row.get(4), &row.get(5), row.get(8)))
            .collect::<Vec<_>>();
        assert_eq!(accepted, [false, true]);
        tx.rollback().await?;
        Ok(())
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
}
