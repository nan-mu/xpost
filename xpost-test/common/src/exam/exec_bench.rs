//! EXB2 batches with DUT Metrics snapshots.
//!
//! Flash each carrier once. Install one XDP ELF per program and mode.
//! Send 8258 bytes and invoke once for each batch. The workload runs its own rounds.
//! Check the firmware's return code and the complete output against v0.1.1.
//! Arm Metrics before the first batch. Freeze after the last batch.
//! Save the raw buffers, counter boundaries, and invocation ticks under one run ID.

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

use crate::agent::{AgentClient, ExecBatch};
use crate::config::{ExecBenchConfig, ExecBenchSampling, FileConfig};
use crate::db::{Collection, Database, metrics, protocol};

use super::exb2::{self, BUF, ROUNDS, SEED, Sampling};
use super::medium::{Actions, Resume};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Hal,
    Aot,
    Jit,
    Rbpf,
}

impl Mode {
    pub(super) fn parse(name: &str) -> Result<Self> {
        match name {
            "hal" => Ok(Self::Hal),
            "aot" | "ebpf" | "xdp" => Ok(Self::Aot),
            "jit" => Ok(Self::Jit),
            "rbpf" => Ok(Self::Rbpf),
            _ => bail!("unknown EXB2 mode `{name}`"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Hal => "hal",
            Self::Aot => "aot",
            Self::Jit => "jit",
            Self::Rbpf => "rbpf",
        }
    }

    pub(super) fn firmware(self, program: &str) -> String {
        match self {
            Self::Hal => format!("exec-bench-hal-{program}"),
            Self::Aot | Self::Jit => "exec-bench-aot".into(),
            Self::Rbpf => "exec-bench-rbpf".into(),
        }
    }

    pub(super) fn programs(self) -> &'static [&'static str] {
        match self {
            Self::Hal => &exb2::HAL_PROGRAMS,
            Self::Aot | Self::Jit | Self::Rbpf => exb2::xdp_programs(),
        }
    }

    fn deployment(self, program: &str) -> Option<String> {
        match self {
            Self::Hal => None,
            Self::Aot => Some(program.to_owned()),
            _ => Some(format!("{program}_{}", self.as_str())),
        }
    }
}

struct Target {
    mode: Mode,
    firmware: String,
    programs: Vec<&'static str>,
    expected_hash: Option<String>,
}

fn standalone_targets(config: &ExecBenchConfig) -> Result<Vec<Target>> {
    let mut selected = Vec::new();
    for mode in &config.modes {
        let mode = Mode::parse(mode)?;
        if !selected.contains(&mode) {
            selected.push(mode);
        }
    }
    ensure!(!selected.is_empty(), "select at least one EXB2 mode");
    for program in &config.workloads {
        ensure!(
            selected
                .iter()
                .any(|mode| mode.programs().contains(&program.as_str())),
            "no selected EXB2 mode supports workload `{program}`"
        );
    }
    let mut targets = Vec::new();
    for mode in selected {
        let programs: Vec<_> = mode
            .programs()
            .iter()
            .copied()
            .filter(|name| {
                config.workloads.is_empty()
                    || config.workloads.iter().any(|selected| selected == name)
            })
            .collect();
        if mode == Mode::Hal {
            for program in programs {
                targets.push(Target {
                    mode,
                    firmware: mode.firmware(program),
                    programs: vec![program],
                    expected_hash: None,
                });
            }
        } else if let Some(first) = programs.first() {
            targets.push(Target {
                mode,
                firmware: mode.firmware(first),
                programs,
                expected_hash: None,
            });
        }
    }
    // AOT and JIT share one carrier. Keep them adjacent.
    targets.sort_by(|a, b| a.firmware.cmp(&b.firmware));
    Ok(targets)
}

fn standalone_sampling(config: &ExecBenchConfig) -> Result<Sampling> {
    match config.sampling {
        ExecBenchSampling::Medium => Ok(Sampling::MEDIUM),
        ExecBenchSampling::Full => {
            ensure!(
                !config.modes.is_empty() && config.modes.iter().all(|mode| mode == "hal"),
                "standalone EXB2 Full requires HAL mode"
            );
            Ok(Sampling::FULL)
        }
    }
}

pub async fn run_exec_bench(cfg: &FileConfig) -> Result<()> {
    let sampling = standalone_sampling(&cfg.exec_bench)?;
    if sampling == Sampling::FULL {
        super::medium::revision().context("build the Full runner with XPOST_MEDIUM_REVISION")?;
    }
    let mut targets = standalone_targets(&cfg.exec_bench)?;
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("exec_bench needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    agent.health().await?;
    for target in &mut targets {
        if target.mode == Mode::Hal {
            let hash = agent.preflight_hal_exec(&target.firmware).await?;
            parse_sha256(&hash)?;
            target.expected_hash = Some(hash);
        } else {
            for program in &target.programs {
                if let Some(profile) = target.mode.deployment(program) {
                    agent.preflight_xdp(&profile).await?;
                }
            }
        }
    }
    let settings = Database::load()?;
    let database = if sampling == Sampling::FULL {
        settings.prepare_append_only().await?
    } else {
        settings.prepare().await?
    };
    database
        .protect(run_selected(&agent, &database, targets, sampling))
        .await
}

async fn run_selected(
    agent: &AgentClient,
    database: &Collection<'_>,
    targets: Vec<Target>,
    sampling: Sampling,
) -> Result<()> {
    let mut db = database.connect().await?;
    let mut loaded = String::new();
    let mut image = String::new();
    let mut order = 0;
    for target in targets {
        if image != target.firmware {
            let flash = agent
                .flash_fw_evidence(&target.firmware, target.expected_hash.as_deref())
                .await?;
            ensure!(
                flash["firmware_profile"] == target.firmware,
                "Agent flashed an unexpected firmware profile"
            );
            loaded = flash["firmware_sha256"]
                .as_str()
                .context("missing Firmware ELF digest")?
                .to_owned();
            ensure!(
                target
                    .expected_hash
                    .as_ref()
                    .is_none_or(|hash| hash == &loaded),
                "Agent flashed an unexpected Firmware ELF digest"
            );
            agent.get_init().await?;
            image = target.firmware;
        }
        run_mode(
            agent,
            &mut db,
            database,
            target.mode,
            &target.programs,
            &loaded,
            None,
            &mut order,
            sampling,
            None,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn run_mode(
    agent: &AgentClient,
    db: &mut Client,
    database: &Collection<'_>,
    mode: Mode,
    programs: &[&'static str],
    firmware_hash: &str,
    medium_id: Option<Uuid>,
    order: &mut i32,
    sampling: exb2::Sampling,
    mut formal: Option<(&Resume, &mut Option<String>, &mut Actions)>,
) -> Result<()> {
    let profile = parse_sha256(firmware_hash)?;
    for &program in programs {
        let repeats = (1..=sampling.runs)
            .filter(|&repeat| {
                formal.as_ref().is_none_or(|(resume, _, _)| {
                    resume.should_run("exec_bench", mode.as_str(), "metrics", program, repeat)
                })
            })
            .collect::<Vec<_>>();
        if repeats.is_empty() {
            continue;
        }
        if let Some(name) = mode.deployment(program) {
            let loaded = formal.as_ref().and_then(|(_, loaded, _)| loaded.as_deref());
            if loaded != Some(&name) {
                let install_started = std::time::Instant::now();
                agent
                    .bind_xdp(&name, &mode.firmware(program), firmware_hash)
                    .await?;
                if let Some((_, loaded, actions)) = &mut formal {
                    actions.cold_install_confirmed += 1;
                    actions.cold_install_s += install_started.elapsed().as_secs_f64();
                    **loaded = Some(name);
                }
                agent.get_init().await?;
            }
        }
        let (n, input, expected) = exb2::request(exb2::task(program));
        let sample = Sample {
            mode,
            program,
            n,
            input,
            expected,
            profile,
            medium_id,
            sampling,
        };
        for repeat in repeats {
            let attempt = Attempt {
                id: Uuid::new_v4(),
                order: *order,
                repeat,
                config: Uuid::new_v4(),
                retry_of: formal.as_ref().and_then(|(resume, _, _)| {
                    resume.retry_of("exec_bench", mode.as_str(), "metrics", program, repeat)
                }),
                medium_identity: formal.as_ref().and_then(|(resume, _, _)| {
                    resume.identity("exec_bench", mode.as_str(), "metrics", program, repeat)
                }),
            };
            *order += 1;
            run_attempt(agent, db, database, &sample, &attempt).await?;
        }
    }
    Ok(())
}

fn parse_sha256(hex: &str) -> Result<[u8; 32]> {
    ensure!(
        hex.len() == 64 && hex.as_bytes().iter().all(u8::is_ascii_hexdigit),
        "invalid Firmware ELF digest"
    );
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * index..][..2], 16)?;
    }
    Ok(digest)
}

struct Sample {
    mode: Mode,
    program: &'static str,
    n: u32,
    input: Vec<u8>,
    expected: Vec<u8>,
    profile: [u8; 32],
    medium_id: Option<Uuid>,
    sampling: Sampling,
}

impl Sample {
    fn phase(&self) -> &'static str {
        if self.medium_id.is_some() || self.sampling == Sampling::FULL {
            "formal"
        } else {
            "pilot"
        }
    }

    fn hal_build(&self) -> Option<Value> {
        (self.mode == Mode::Hal).then(|| {
            json!({
                "implementation": "native_rust",
                "firmware_profile": self.mode.firmware(self.program),
                "cargo_feature": self.program,
                "cargo_profile": "release",
                "opt_level": "s",
                "lto": "fat",
                "codegen_units": 1,
                "timing_boundary": "EXB2 execute entry through response publication",
            })
        })
    }
}

struct Attempt<'a> {
    id: Uuid,
    order: i32,
    repeat: i32,
    config: Uuid,
    retry_of: Option<Uuid>,
    medium_identity: Option<&'a Value>,
}

async fn run_attempt(
    agent: &AgentClient,
    db: &mut Client,
    database: &Collection<'_>,
    sample: &Sample,
    attempt: &Attempt<'_>,
) -> Result<()> {
    if let Err(error) = start(db, attempt, sample).await {
        let state = protocol::run_status(database, attempt.id).await;
        println!(
            "XPOST_RESULT {}",
            result_event(
                attempt,
                sample,
                "blocked",
                &state,
                None,
                &json!([format!("{error:#}")])
            )
        );
        return Err(error);
    }
    let acquired = collect(agent, attempt.id, sample).await;
    let evidence = validate(attempt.id, sample, &acquired);
    let status = evidence.status(sample.sampling);
    let mut terminal = finish(db, attempt, sample, &evidence).await;
    let mut event = result_event(
        attempt,
        sample,
        status,
        status,
        Some(&evidence),
        &evidence.issues,
    );
    if let Err(error) = &terminal {
        let initial_error = format!("{error:#}");
        let mut state = protocol::run_status(database, attempt.id).await;
        if state == "running"
            && let Ok(mut recovered) = database.connect().await
        {
            terminal = finish(&mut recovered, attempt, sample, &evidence).await;
            state = protocol::run_status(database, attempt.id).await;
            if state == status {
                *db = recovered;
            }
        }
        event["database_status"] = json!(state);
        event["write_error"] = json!(initial_error);
        if state != status {
            event["status"] = json!("write_failed");
            event["qualified"] = json!(false);
            let saved = preserve(attempt.id, sample, &evidence, &event);
            event["evidence"] = match saved {
                Ok(path) => json!(path),
                Err(save) => json!(format!("local export failed: {save:#}")),
            };
            println!("XPOST_RESULT {event}");
            terminal.with_context(|| format!("persist EXB2 run_id={}", attempt.id))?;
            bail!("unconfirmed EXB2 database status for {}", attempt.id);
        }
    }
    println!("XPOST_RESULT {event}");
    ensure!(
        acquired.session_released,
        "EXB2 run_id={} perf session needs recovery: {}",
        attempt.id,
        evidence.issues,
    );
    Ok(())
}

#[derive(Default)]
struct Acquired {
    batches: Vec<Result<ExecBatch>>,
    counters: Option<Vec<u8>>,
    boundaries: Option<(u32, u64, u64, u64)>,
    session_released: bool,
    issues: Vec<String>,
}

async fn collect(agent: &AgentClient, run: Uuid, sample: &Sample) -> Acquired {
    let mut acquired = Acquired::default();
    let result: Result<()> = async {
        let (epoch, boot) = agent
            .perf_prepare(run, &protocol::hex(&sample.profile), "metrics")
            .await?;
        let arm = agent.perf_arm(epoch).await;
        if arm.is_ok() {
            let input = protocol::hex(&sample.input);
            for _ in 0..sample.sampling.samples {
                acquired.batches.push(agent.exb2(&input).await);
            }
        }
        if let Err(error) = &arm {
            acquired.issues.push(format!("Metrics arm: {error:#}"));
        }
        let frozen = agent.perf_freeze(epoch).await?;
        if let Ok(arm) = arm {
            acquired.boundaries = Some((epoch, boot, arm, frozen.tick));
        }
        if frozen.flags != 0 {
            acquired
                .issues
                .push(format!("Metrics firmware flags: {}", frozen.flags));
        }
        let read = agent.perf_read(epoch, frozen).await;
        if let Ok(bytes) = read {
            acquired.counters = Some(bytes);
        } else if let Err(error) = read {
            acquired.issues.push(format!("Metrics read: {error:#}"));
        }
        agent.perf_release(epoch).await?;
        acquired.session_released = true;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        acquired.issues.push(format!("Metrics session: {error:#}"));
    }
    acquired
}

fn validate<'a>(run: Uuid, sample: &Sample, acquired: &'a Acquired) -> Completed<'a> {
    let mut issues = acquired.issues.clone();
    let counters = acquired.counters.as_deref();
    let pair = counters.and_then(|bytes| {
        let validated = metrics::decode(bytes).and_then(|pair| {
            let (epoch, boot, arm, freeze) = acquired
                .boundaries
                .context("Metrics has no session boundaries")?;
            pair.require(run, epoch, boot, &sample.profile, arm)?;
            ensure!(
                pair.final_.tick == freeze,
                "Metrics freeze tick differs from export"
            );
            ensure!(
                pair.baseline.hz == 1_000_000 && pair.baseline.bits == 64,
                "invalid EXB2 Metrics clock"
            );
            Ok(pair)
        });
        match validated {
            Ok(pair) => Some(pair),
            Err(error) => {
                issues.push(format!("{error:#}"));
                None
            }
        }
    });
    if let Some(pair) = &pair
        && !all_pass(pair, sample.sampling.samples as u64)
    {
        issues.push(format!(
            "EXB2 Metrics does not show exactly {} successful invocation(s)",
            sample.sampling.samples
        ));
    }
    // The run-wide totals must account for all sequential requests with zero faults.
    // If they disagree, a reply alone cannot establish counter attribution.
    let metrics_ok = pair.is_some() && issues.is_empty();
    let boot = pair.as_ref().map(|pair| pair.baseline.boot).or_else(|| {
        acquired
            .batches
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .map(|reply| u64::from(reply.boot_id))
            .find(|boot| *boot != 0)
    });
    let mut previous_end = None;
    let mut batches = Vec::with_capacity(acquired.batches.len());
    for (index, result) in acquired.batches.iter().enumerate() {
        let mut batch = Batch::default();
        match result {
            Ok(reply) => {
                batch.reply = Some(reply);
                batch.result_ok = verified(sample, reply);
                batch.recordable = boot == Some(u64::from(reply.boot_id))
                    && reply.output.len() == BUF
                    && reply.begin_tick <= reply.end_tick
                    && field(reply, 40) <= ROUNDS;
                if !batch.result_ok {
                    issues.push(format!(
                        "EXB2 batch {index}: return, clock, or complete output mismatched"
                    ));
                }
                if let Some(pair) = &pair {
                    let in_window = u64::from(reply.boot_id) == pair.baseline.boot
                        && pair.baseline.tick <= reply.begin_tick
                        && reply.begin_tick <= reply.end_tick
                        && reply.end_tick <= pair.final_.tick
                        && previous_end.is_none_or(|end| end <= reply.begin_tick);
                    if in_window {
                        previous_end = Some(reply.end_tick);
                    } else {
                        issues.push(format!("EXB2 batch {index}: invocation is outside its ordered Metrics clock window"));
                    }
                    batch.qualified = metrics_ok && in_window && batch.result_ok;
                }
            }
            Err(error) => issues.push(format!("EXB2 batch {index} invocation: {error:#}")),
        }
        batches.push(batch);
    }
    Completed {
        batches,
        boot,
        counters,
        pair,
        issues: json!(issues),
    }
}

fn result_event(
    attempt: &Attempt<'_>,
    sample: &Sample,
    status: &str,
    database_status: &str,
    evidence: Option<&Completed<'_>>,
    issues: &Value,
) -> Value {
    let (completed, valid) = evidence.map_or((0, 0), Completed::counts);
    let batches = evidence.map_or(&[][..], |evidence| &evidence.batches);
    let single = if sample.sampling.samples == 1 {
        batches.first().and_then(|batch| batch.reply)
    } else {
        None
    };
    let batch_us = if sample.sampling.samples == 1 {
        json!(single.and_then(|reply| reply.end_tick.checked_sub(reply.begin_tick)))
    } else if valid > 0 {
        let total: u128 = batches
            .iter()
            .filter(|batch| batch.qualified)
            .filter_map(|batch| batch.reply)
            .map(|reply| u128::from(reply.end_tick - reply.begin_tick))
            .sum();
        json!(total as f64 / f64::from(valid))
    } else {
        Value::Null
    };
    let us_per_round = if status == "complete" {
        batch_us.as_f64().map(|us| us / f64::from(ROUNDS))
    } else {
        None
    };
    json!({ "run_id": attempt.id.to_string(), "experiment": "exec_bench", "mode": sample.mode.as_str(),
        "variant": "metrics", "phase": sample.phase(), "medium_id": sample.medium_id, "stage": sample.medium_id.map(|_| crate::config::MediumStage::for_mode(sample.mode.as_str())), "retry_of": attempt.retry_of, "input": { "program": sample.program, "n": sample.n, "batch_count": ROUNDS, "seed": SEED },
        "repeat": attempt.repeat, "status": status, "database_status": database_status, "issues": issues,
        "firmware_sha256": protocol::hex(&sample.profile), "hal_build": sample.hal_build(),
        "batch_us": batch_us, "us_per_round": us_per_round, "execution_return": single.map(|reply| reply.execution_return),
        "batches_expected": sample.sampling.samples, "batches_completed": completed,
        "valid_batches": valid, "qualified": status == "complete",
    })
}

fn preserve(run: Uuid, sample: &Sample, evidence: &Completed<'_>, event: &Value) -> Result<String> {
    use std::fs;
    let root = std::path::Path::new("exb2-evidence");
    fs::create_dir_all(root)?;
    let path = root.join(run.to_string());
    fs::create_dir(&path)?;
    fs::write(path.join("input.bin"), &sample.input)?;
    fs::write(path.join("expected.bin"), &sample.expected)?;
    let mut manifest = Vec::new();
    for (index, batch) in evidence.batches.iter().enumerate() {
        if let Some(reply) = batch.reply {
            let name = if sample.sampling.samples == 1 {
                "actual.bin".into()
            } else {
                format!("actual-{index}.bin")
            };
            fs::write(path.join(&name), &reply.output)?;
            let mut entry = reply_facts(index, reply);
            entry["actual_path"] = json!(name);
            entry["qualified"] = json!(batch.qualified);
            manifest.push(entry);
        }
    }
    fs::write(
        path.join("batches.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    if let Some(counters) = evidence.counters {
        fs::write(path.join("metrics.bin"), counters)?;
    }
    fs::write(path.join("attempt.json"), serde_json::to_vec_pretty(event)?)?;
    Ok(path.display().to_string())
}

fn all_pass(pair: &metrics::Pair<'_>, count: u64) -> bool {
    [
        ("exec_batch_invocations", count),
        ("exec_batch_action_pass", count),
        ("exec_batch_action_aborted", 0),
        ("exec_batch_action_drop", 0),
        ("exec_batch_action_tx", 0),
    ]
    .into_iter()
    .all(|(name, expected)| pair.delta(name) == Some(expected))
}

fn verified(sample: &Sample, reply: &ExecBatch) -> bool {
    let expected_action = if sample.mode == Mode::Hal { 0 } else { 2 };
    reply.boot_id != 0
        && reply.begin_tick <= reply.end_tick
        && reply.execution_return == expected_action
        && reply.output == sample.expected
}

async fn start(db: &mut Client, attempt: &Attempt<'_>, sample: &Sample) -> Result<()> {
    let mode = sample.mode.as_str();
    let condition = Uuid::new_v4();
    let config = attempt.config;
    let tx = db.transaction().await?;
    let mut parameters = json!({ "workload": sample.program, "n": sample.n, "batch_count": ROUNDS, "seed": SEED, "medium_id": sample.medium_id, "stage": sample.medium_id.map(|_| crate::config::MediumStage::for_mode(mode)), "medium_identity": attempt.medium_identity, "retry_of": attempt.retry_of });
    if let Some(build) = sample.hal_build() {
        parameters["hal_build"] = build;
    }
    tx.execute("INSERT INTO release.bench_condition(condition_id,experiment,protocol_version,parameters,planned_modes) VALUES($1,'exec_bench','1',$2,ARRAY[$3]::text[])", &[&condition,&parameters,&mode]).await?;
    let request = protocol::create_artifact(
        &tx,
        "exb2_input",
        &sample.input,
        "application/octet-stream",
        None,
    )
    .await?;
    let expected = protocol::create_artifact(
        &tx,
        "exb2_expected",
        &sample.expected,
        "application/octet-stream",
        None,
    )
    .await?;
    let digest = protocol::hex(&sample.profile);
    tx.execute(&format!("INSERT INTO release.{mode}_exec_bench_config(config_id,condition_id,instrumentation,profile_hash,workload,work_param,batch_count,samples_per_run,input_bytes,input_protocol,wifi_enabled,fixture_artifact_id) VALUES($1,$2,'counter_only',$3,$4,$5,$6,$7,8258,'EXB2',false,$8)"), &[&config,&condition,&digest,&sample.program,&(sample.n as i32),&i64::from(ROUNDS),&sample.sampling.samples,&request]).await?;
    let libbpf_commit =
        (sample.mode != Mode::Hal).then_some("07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113");
    tx.execute("INSERT INTO release.bench_run(run_id,condition_id,config_id,mode,experiment,variant,phase,repeat_no,seed,plan_order,status,run_uuid,libbpf_commit,xpost_commit) VALUES($1,$2,$3,$4,'exec_bench','metrics',$5,$6,$7::text::release.bench_u64,$8,'running',$1,$9,$10)", &[&attempt.id,&condition,&config,&mode,&sample.phase(),&attempt.repeat,&SEED.to_string(),&attempt.order,&libbpf_commit,&super::medium::revision()]).await?;
    protocol::link_artifact(&tx, attempt.id, "exb2_input", request).await?;
    protocol::link_artifact(&tx, attempt.id, "exb2_expected", expected).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Default)]
struct Batch<'a> {
    reply: Option<&'a ExecBatch>,
    recordable: bool,
    result_ok: bool,
    qualified: bool,
}

struct Completed<'a> {
    batches: Vec<Batch<'a>>,
    boot: Option<u64>,
    counters: Option<&'a [u8]>,
    pair: Option<metrics::Pair<'a>>,
    issues: Value,
}

impl Completed<'_> {
    fn counts(&self) -> (i32, i32) {
        let completed = self.batches.iter().filter(|batch| batch.recordable).count();
        let valid = self.batches.iter().filter(|batch| batch.qualified).count();
        (completed as i32, valid as i32)
    }

    fn status(&self, sampling: Sampling) -> &'static str {
        if self.counts().1 == sampling.samples {
            "complete"
        } else {
            "failed"
        }
    }
}

fn field(reply: &ExecBatch, offset: usize) -> u32 {
    u32::from_le_bytes(reply.output[offset..offset + 4].try_into().unwrap())
}

fn reply_facts(index: usize, reply: &ExecBatch) -> Value {
    json!({ "sample_index": index, "boot_id": reply.boot_id, "begin_tick": reply.begin_tick,
        "end_tick": reply.end_tick, "execution_return": reply.execution_return })
}

async fn finish(
    db: &mut Client,
    attempt: &Attempt<'_>,
    sample: &Sample,
    evidence: &Completed<'_>,
) -> Result<()> {
    let mode = sample.mode.as_str();
    let tx = db.transaction().await?;
    let (completed, valid) = evidence.counts();
    let status = evidence.status(sample.sampling);
    let qualified = status == "complete";
    let boot = evidence.boot.map(|boot| boot.to_string());
    let n = tx.execute("UPDATE release.bench_run SET status=$2,boot_id=$3::text::release.bench_u64,issues=$4 WHERE run_id=$1 AND status='running'",
        &[&attempt.id,&status,&boot,&evidence.issues]).await?;
    ensure!(n == 1, "EXB2 attempt has no running row");
    let snapshots = match &evidence.pair {
        Some(pair) => Some(pair.write(&tx, mode).await?),
        None => None,
    };
    if let Some(raw) = evidence.counters {
        protocol::artifact(&tx, attempt.id, "metrics_wire", raw).await?;
    }
    if snapshots.is_none() && completed > 0 {
        tx.execute("INSERT INTO release.bench_clock(run_id,clock_id,domain,tick_hz,tick_bits,boot_id,precision_description) VALUES($1,'dut','dut',1000000,64,$2::text::release.bench_u64,'ESP HAL microseconds since boot')", &[&attempt.id,&boot]).await?;
    }
    let insert_batch = tx.prepare(&format!("INSERT INTO release.{mode}_exec_batch(run_id,sample_index,clock_id,begin_tick,end_tick,batch_count,completed_iterations,seed,input_bytes,expected_bytes,actual_bytes,execution_return,status,result,digest,result_ok,qualified) VALUES($1,$2,'dut',$3::text::release.bench_u64,$4::text::release.bench_u64,$5,$6,$7::text::release.bench_u64,$8,$9,$10,$11,$12,$13::text::release.bench_u64,$14,$15,$16)")).await?;
    let mut unrecorded = Vec::new();
    for (index, batch) in evidence.batches.iter().enumerate() {
        let Some(reply) = batch.reply else { continue };
        if sample.sampling.samples == 1 || !batch.recordable {
            let role = if sample.sampling.samples == 1 {
                "exb2_actual".into()
            } else {
                format!("exb2_actual_{index}")
            };
            protocol::artifact(&tx, attempt.id, &role, &reply.output).await?;
            if !batch.recordable {
                let mut facts = reply_facts(index, reply);
                facts["actual_role"] = json!(role);
                unrecorded.push(facts);
            }
        }
        if batch.recordable {
            let result = (u64::from(field(reply, 48)) << 32) | u64::from(field(reply, 44));
            tx.execute(
                &insert_batch,
                &[
                    &attempt.id,
                    &(index as i32),
                    &reply.begin_tick.to_string(),
                    &reply.end_tick.to_string(),
                    &i64::from(ROUNDS),
                    &i64::from(field(reply, 40)),
                    &SEED.to_string(),
                    &sample.input,
                    &sample.expected,
                    &reply.output,
                    &i64::from(reply.execution_return),
                    &i64::from(field(reply, 36)),
                    &result.to_string(),
                    &i64::from(field(reply, 52)),
                    &batch.result_ok,
                    &batch.qualified,
                ],
            )
            .await?;
        }
    }
    if !unrecorded.is_empty() {
        let bytes = serde_json::to_vec(&unrecorded)?;
        protocol::artifact(&tx, attempt.id, "exb2_unrecorded_replies", &bytes).await?;
    }

    let analysis = Uuid::new_v4();
    let source_digest = protocol::digest(
        evidence
            .counters
            .or_else(|| {
                evidence
                    .batches
                    .iter()
                    .find_map(|batch| batch.reply.map(|reply| reply.output.as_slice()))
            })
            .unwrap_or(&sample.input),
    );
    tx.execute("INSERT INTO release.bench_analysis(analysis_id,run_id,algorithm_version,source_digest,qualified,issues) VALUES($1,$2,$6,$3,$4,$5)",
        &[&analysis,&attempt.id,&source_digest,&qualified,&evidence.issues,&exb2::ANALYSIS_VERSION]).await?;
    let baseline = snapshots.map(|(start, _)| start);
    let final_ = snapshots.map(|(_, end)| end);
    tx.execute(&format!("INSERT INTO release.{mode}_exec_bench_metrics_data(run_id,config_id,baseline_snapshot_id,final_snapshot_id,analysis_id,qualified,issues,batches_expected,batches_completed,valid_batches) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"),
        &[&attempt.id,&attempt.config,&baseline,&final_,&analysis,&qualified,&evidence.issues,&sample.sampling.samples,&completed,&valid]).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_sampling_defaults_to_medium_and_requires_hal_for_full() -> Result<()> {
        let medium: ExecBenchConfig = toml::from_str("modes = ['hal', 'aot']")?;
        let full: ExecBenchConfig = toml::from_str("modes = ['hal']\nsampling = 'full'")?;
        assert!(standalone_sampling(&medium)? == Sampling::MEDIUM);
        assert!(standalone_sampling(&full)? == Sampling::FULL);
        assert_eq!(standalone_targets(&full)?.len(), 8);
        assert_eq!(
            fixture_sample(Mode::Hal, standalone_sampling(&medium)?).phase(),
            "pilot"
        );
        assert_eq!(
            fixture_sample(Mode::Hal, standalone_sampling(&full)?).phase(),
            "formal"
        );
        let mixed: ExecBenchConfig = toml::from_str("modes = ['hal', 'aot']\nsampling = 'full'")?;
        assert!(standalone_sampling(&mixed).is_err());
        Ok(())
    }

    #[test]
    fn standalone_selects_the_hal_matmul_profile_and_rejects_non_hal_matmul() -> Result<()> {
        let mut config = ExecBenchConfig {
            workloads: vec!["matmul".into()],
            ..ExecBenchConfig::default()
        };
        let targets = standalone_targets(&config)?;
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].firmware, "exec-bench-hal-matmul");
        assert_eq!(targets[0].programs, ["matmul"]);
        config.modes.remove(0);
        assert!(standalone_targets(&config).is_err());
        Ok(())
    }

    #[test]
    fn full_run_qualifies_batches_individually_under_exact_perf_totals() {
        let sample = fixture_sample(Mode::Hal, Sampling::FULL);
        let run = Uuid::new_v4();
        let mut acquired = fixture_acquisition(run, &sample, 100);
        acquired.batches[51].as_mut().unwrap().output[BUF - 1] ^= 1;
        acquired.batches[70].as_mut().unwrap().output.pop();
        acquired.batches[80] = Err(anyhow::anyhow!("missing reply"));
        let evidence = validate(run, &sample, &acquired);
        assert_eq!(evidence.counts(), (98, 97));
        assert_eq!(evidence.status(sample.sampling), "failed");
        assert!(!evidence.batches[51].qualified);
        assert!(evidence.batches[81].qualified);

        let acquired = fixture_acquisition(run, &sample, 101);
        assert_eq!(validate(run, &sample, &acquired).counts(), (100, 0));
    }

    #[tokio::test]
    #[ignore = "requires a disposable protocol database XPOST_CASE_TEST_DB_FILE"]
    async fn exb2_attempt_roundtrips_in_all_four_protocol_schemas() -> Result<()> {
        let settings = Database::load_fixture()?;
        let database = settings.resume().await?;
        let mut client = database.connect().await?;
        for mode in [Mode::Hal, Mode::Aot, Mode::Jit, Mode::Rbpf] {
            for sampling in [Sampling::MEDIUM, Sampling::FULL] {
                roundtrip(
                    &database,
                    &mut client,
                    fixture_sample(mode, sampling),
                    false,
                )
                .await?;
            }
        }
        let sample = fixture_sample(Mode::Hal, Sampling::FULL);
        roundtrip(&database, &mut client, sample, true).await?;
        let mut matmul = fixture_sample(Mode::Hal, Sampling::MEDIUM);
        matmul.program = "matmul";
        (matmul.n, matmul.input, matmul.expected) = exb2::request(exb2::task(matmul.program));
        roundtrip(&database, &mut client, matmul, false).await?;
        finish_rolls_back_on_constraint_failure(&database, &mut client).await?;
        Ok(())
    }

    async fn roundtrip(
        database: &Collection<'_>,
        client: &mut Client,
        sample: Sample,
        corrupt: bool,
    ) -> Result<()> {
        let sampling = sample.sampling;
        let mode = sample.mode;
        let attempt = Attempt {
            id: Uuid::new_v4(),
            order: 0,
            repeat: 1,
            config: Uuid::new_v4(),
            retry_of: None,
            medium_identity: None,
        };
        start(client, &attempt, &sample).await?;
        let mut acquired = fixture_acquisition(attempt.id, &sample, sampling.samples as u64);
        if corrupt {
            acquired.batches[51].as_mut().unwrap().output[BUF - 1] ^= 1;
            acquired.batches[70].as_mut().unwrap().output.pop();
        }
        let evidence = validate(attempt.id, &sample, &acquired);
        finish(client, &attempt, &sample, &evidence).await?;
        let mode = mode.as_str();
        let row = client.query_one(&format!("SELECT r.status, c.samples_per_run, d.batches_expected, d.batches_completed, d.valid_batches, d.qualified FROM release.bench_run r JOIN release.{mode}_exec_bench_metrics_data d USING (run_id,config_id) JOIN release.{mode}_exec_bench_config c USING (config_id) WHERE r.run_id=$1"), &[&attempt.id]).await?;
        let status = if corrupt { "failed" } else { "complete" };
        assert_eq!(row.get::<_, String>(0), status);
        assert_eq!(protocol::run_status(database, attempt.id).await, status);
        assert_eq!(row.get::<_, i32>(1), sampling.samples);
        assert_eq!(row.get::<_, i32>(2), sampling.samples);
        assert_eq!(row.get::<_, i32>(3), sampling.samples - i32::from(corrupt));
        assert_eq!(
            row.get::<_, i32>(4),
            sampling.samples - 2 * i32::from(corrupt)
        );
        assert_eq!(row.get::<_, bool>(5), !corrupt);
        let rows = client.query(&format!("SELECT sample_index, actual_bytes, result_ok, qualified FROM release.{mode}_exec_batch WHERE run_id=$1 ORDER BY sample_index"), &[&attempt.id]).await?;
        let expected_indices = (0..sampling.samples).filter(|&index| !corrupt || index != 70);
        assert_eq!(
            rows.iter()
                .map(|row| row.get::<_, i32>(0))
                .collect::<Vec<_>>(),
            expected_indices.collect::<Vec<_>>()
        );
        for row in rows {
            let good = !corrupt || row.get::<_, i32>(0) != 51;
            assert_eq!(row.get::<_, Vec<u8>>(1) == sample.expected, good);
            assert_eq!(row.get::<_, bool>(2), good);
            assert_eq!(row.get::<_, bool>(3), good);
        }
        if sample.program == "matmul" {
            let stored = client
                .query_one(
                    "SELECT c.workload,c.work_param,c.profile_hash,i.original_bytes,b.actual_bytes,
                 clk.tick_hz::text,b.begin_tick::text,b.end_tick::text,
                 bc.parameters->'hal_build'->>'cargo_feature',r.libbpf_commit
                 FROM release.hal_exec_bench_config c
                 JOIN release.bench_condition bc USING (condition_id)
                 JOIN release.bench_run r USING (config_id)
                 JOIN release.bench_artifact i ON i.artifact_id=c.fixture_artifact_id
                 JOIN release.hal_exec_batch b USING (run_id)
                 JOIN release.bench_clock clk USING (run_id)
                 WHERE r.run_id=$1 AND clk.clock_id='dut'",
                    &[&attempt.id],
                )
                .await?;
            assert_eq!(stored.get::<_, String>(0), "matmul");
            assert_eq!(stored.get::<_, i32>(1), 2);
            assert_eq!(stored.get::<_, String>(2), protocol::hex(&sample.profile));
            assert_eq!(stored.get::<_, Vec<u8>>(3), sample.input);
            assert_eq!(stored.get::<_, Vec<u8>>(4), sample.expected);
            assert_eq!(stored.get::<_, String>(5), "1000000");
            assert_eq!(stored.get::<_, String>(6), "110");
            assert_eq!(stored.get::<_, String>(7), "125");
            assert_eq!(stored.get::<_, String>(8), "matmul");
            assert_eq!(stored.get::<_, Option<String>>(9), None);
        }
        let metrics = client
            .query_one(
                &format!("SELECT count(*) FROM release.{mode}_metric_snapshot WHERE run_id=$1"),
                &[&attempt.id],
            )
            .await?;
        assert_eq!(metrics.get::<_, i64>(0), 2);
        let event = result_event(
            &attempt,
            &sample,
            status,
            status,
            Some(&evidence),
            &evidence.issues,
        );
        assert_eq!(event["batch_us"].as_f64(), Some(15.0));
        assert_eq!(
            event["valid_batches"],
            sampling.samples - 2 * i32::from(corrupt)
        );
        assert_eq!(event["us_per_round"].as_f64(), (!corrupt).then_some(5.0));
        Ok(())
    }

    async fn finish_rolls_back_on_constraint_failure(
        database: &Collection<'_>,
        client: &mut Client,
    ) -> Result<()> {
        let sample = fixture_sample(Mode::Hal, Sampling::MEDIUM);
        let attempt = Attempt {
            id: Uuid::new_v4(),
            order: 0,
            repeat: 1,
            config: Uuid::new_v4(),
            retry_of: None,
            medium_identity: None,
        };
        start(client, &attempt, &sample).await?;
        client.execute(
            "INSERT INTO release.bench_clock(run_id,clock_id,domain,tick_hz,tick_bits,precision_description)
             VALUES ($1,'dut','dut',1000000,64,'fixture conflict')",
            &[&attempt.id],
        ).await?;
        let acquired = fixture_acquisition(attempt.id, &sample, sample.sampling.samples as u64);
        let evidence = validate(attempt.id, &sample, &acquired);
        let error = finish(client, &attempt, &sample, &evidence)
            .await
            .unwrap_err();
        let error = error.downcast_ref::<tokio_postgres::Error>().unwrap();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
        );
        assert_eq!(protocol::run_status(database, attempt.id).await, "running");
        let rows = client
            .query_one(
                "SELECT count(*) FROM release.hal_exec_batch WHERE run_id=$1",
                &[&attempt.id],
            )
            .await?;
        assert_eq!(rows.get::<_, i64>(0), 0);
        Ok(())
    }

    fn fixture_sample(mode: Mode, sampling: Sampling) -> Sample {
        let (n, input, expected) = exb2::request(0);
        Sample {
            mode,
            program: "noop",
            n,
            input,
            expected,
            profile: [0x2a; 32],
            medium_id: None,
            sampling,
        }
    }

    fn fixture_acquisition(run: Uuid, sample: &Sample, invocations: u64) -> Acquired {
        let freeze = 110 + 20 * sample.sampling.samples as u64;
        Acquired {
            batches: (0..sample.sampling.samples as u64)
                .map(|index| {
                    Ok(ExecBatch {
                        boot_id: 11,
                        begin_tick: 110 + 20 * index,
                        end_tick: 125 + 20 * index,
                        execution_return: if sample.mode == Mode::Hal { 0 } else { 2 },
                        output: sample.expected.clone(),
                    })
                })
                .collect(),
            counters: Some(
                [
                    synthetic(run, sample, 1, 100, 7),
                    synthetic(run, sample, 2, freeze, 7 + invocations),
                ]
                .concat(),
            ),
            boundaries: Some((3, 11, 100, freeze)),
            session_released: true,
            issues: Vec::new(),
        }
    }

    fn synthetic(run: Uuid, sample: &Sample, phase: u8, tick: u64, count: u64) -> Vec<u8> {
        let mut pfr = b"PFR1\x02\0\0\0\x05\0".to_vec();
        for (name, count) in [
            ("exec_batch_invocations", count),
            ("exec_batch_action_pass", count),
            ("exec_batch_action_aborted", 0),
            ("exec_batch_action_drop", 0),
            ("exec_batch_action_tx", 0),
        ] {
            pfr.push(name.len() as u8);
            pfr.extend_from_slice(name.as_bytes());
            pfr.push(1);
            pfr.extend_from_slice(&count.to_le_bytes());
        }
        let mut frame = b"NMS1\x01\0".to_vec();
        frame.extend([phase, 64]);
        frame.extend((pfr.len() as u32).to_le_bytes());
        frame.extend(run.as_bytes());
        frame.extend(3_u32.to_le_bytes());
        frame.extend(11_u64.to_le_bytes());
        frame.extend(sample.profile);
        frame.extend(tick.to_le_bytes());
        frame.extend(1_000_000_u32.to_le_bytes());
        frame.extend(pfr);
        frame
    }
}
