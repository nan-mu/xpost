//! EXB2 batches with DUT Metrics snapshots.
//!
//! Flash each carrier once. Install one XDP ELF per program and mode.
//! Send 8258 bytes and invoke once for each batch. The workload runs its own rounds.
//! Check the firmware's return code and the complete output against v0.1.1.
//! Arm Metrics before invoking. Freeze after the firmware returns.
//! Save the raw buffers, counter boundaries, and invocation ticks under one run ID.

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio_postgres::{Client, Transaction};
use uuid::Uuid;

use crate::agent::{AgentClient, ExecBatch};
use crate::config::{FileConfig, PostgresConfig};
use crate::db::{metrics, protocol};

use super::exb2::{self, BUF, ROUNDS, SEED};
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

    fn firmware(self) -> &'static str {
        match self {
            Self::Hal => "exec-bench-hal",
            Self::Aot | Self::Jit => "exec-bench-aot",
            Self::Rbpf => "exec-bench-rbpf",
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

pub async fn run_exec_bench(cfg: &FileConfig) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("exec_bench needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    agent.health().await?;
    let mut selected = Vec::new();
    for mode in &cfg.exec_bench.modes {
        let mode = Mode::parse(mode)?;
        if !selected.contains(&mode) {
            selected.push(mode);
        }
    }
    ensure!(!selected.is_empty(), "select at least one EXB2 mode");
    // Native AOT and JIT share the carrier. Keep them adjacent when both are selected.
    selected.sort_by_key(|mode| mode.firmware());
    let mut tables = [
        "bench.condition",
        "bench.run",
        "bench.artifact",
        "bench.run_artifact",
        "bench.clock",
        "bench.analysis",
    ]
    .map(str::to_owned)
    .to_vec();
    for mode in &selected {
        for table in [
            "exec_bench_config",
            "exec_batch",
            "exec_bench_metrics_data",
            "metric_definition",
            "metric_series",
            "metric_snapshot",
            "scalar_sample",
        ] {
            tables.push(format!("{}.{table}", mode.as_str()));
        }
        for program in exb2::PROGRAMS {
            if let Some(profile) = mode.deployment(program) {
                agent.preflight_xdp(&profile).await?;
            }
        }
    }
    let mut db = protocol::connect(&cfg.postgres).await?;
    protocol::check(&mut db, &tables).await?;
    let mut loaded = String::new();
    let mut image = None;
    let mut order = 0;
    for mode in selected {
        if image != Some(mode.firmware()) {
            let flash = agent.flash_fw_evidence(mode.firmware(), None).await?;
            loaded = flash["firmware_sha256"]
                .as_str()
                .context("missing Firmware ELF digest")?
                .to_owned();
            agent.get_init().await?;
            image = Some(mode.firmware());
        }
        run_mode(
            &agent,
            &mut db,
            &cfg.postgres,
            mode,
            &loaded,
            None,
            &mut order,
            None,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn run_mode(
    agent: &AgentClient,
    db: &mut Client,
    database: &PostgresConfig,
    mode: Mode,
    firmware_hash: &str,
    medium_id: Option<Uuid>,
    order: &mut i32,
    mut formal: Option<(&Resume, &mut Option<String>, &mut Actions)>,
) -> Result<()> {
    let profile = parse_sha256(firmware_hash)?;
    for (task, program) in exb2::PROGRAMS.into_iter().enumerate() {
        let repeats = (1..=2)
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
                agent
                    .bind_xdp(&name, mode.firmware(), firmware_hash)
                    .await?;
                if let Some((_, loaded, actions)) = &mut formal {
                    actions.cold_install_confirmed += 1;
                    **loaded = Some(name);
                }
                agent.get_init().await?;
            }
        }
        let (n, input, expected) = exb2::request(task);
        let sample = Sample {
            mode,
            program,
            n,
            input,
            expected,
            profile,
            medium_id,
        };
        for repeat in repeats {
            let index = *order;
            *order += 1;

            let retry_of = formal.as_ref().and_then(|(resume, _, _)| {
                resume.retry_of("exec_bench", mode.as_str(), "metrics", program, repeat)
            });
            run_attempt(agent, db, database, &sample, repeat, index, retry_of).await?;
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
}

struct Attempt {
    id: Uuid,
    order: i32,
    repeat: i32,
    config: Uuid,
    retry_of: Option<Uuid>,
}

async fn run_attempt(
    agent: &AgentClient,
    db: &mut Client,
    database: &PostgresConfig,
    sample: &Sample,
    repeat: i32,
    order: i32,
    retry_of: Option<Uuid>,
) -> Result<()> {
    let attempt = Attempt {
        id: Uuid::new_v4(),
        order,
        repeat,
        config: Uuid::new_v4(),
        retry_of,
    };
    if let Err(error) = start(db, &attempt, sample).await {
        let state = protocol::run_status(database, attempt.id).await;
        println!(
            "XPOST_RESULT {}",
            result_event(
                &attempt,
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
    let mut issues = acquired.issues;
    let pair = match acquired.counters.as_deref() {
        Some(bytes) => match metrics::decode(bytes).and_then(|pair| {
            let (epoch, boot, arm, freeze) = acquired
                .boundaries
                .context("Metrics has no session boundaries")?;
            pair.require(attempt.id, epoch, boot, &sample.profile, arm)?;
            ensure!(
                pair.final_.tick == freeze,
                "Metrics freeze tick differs from export"
            );
            if let Some(reply) = &acquired.reply {
                ensure!(
                    u64::from(reply.boot_id) == boot
                        && pair.baseline.hz == 1_000_000
                        && pair.baseline.bits == 64
                        && arm <= reply.begin_tick
                        && reply.end_tick <= freeze,
                    "EXB2 invocation is outside the Metrics clock window"
                );
            }
            Ok(pair)
        }) {
            Ok(pair) => Some(pair),
            Err(error) => {
                issues.push(format!("{error:#}"));
                None
            }
        },
        None => None,
    };
    if let Some(reply) = &acquired.reply
        && !verified(sample, reply)
    {
        issues.push("EXB2 return, clock, or complete output mismatched".into());
    }
    if let Some(pair) = &pair
        && !one_pass(pair)
    {
        issues.push("EXB2 Metrics does not show one successful invocation".into());
    }
    let status = if issues.is_empty() && pair.is_some() && acquired.reply.is_some() {
        "complete"
    } else {
        "failed"
    };
    let issues = json!(issues);
    let evidence = Completed {
        reply: acquired.reply.as_ref(),
        counters: acquired.counters.as_deref(),
        pair,
        status,
        issues: &issues,
    };
    let mut terminal = finish(db, &attempt, sample, &evidence).await;
    let mut event = result_event(&attempt, sample, status, status, evidence.reply, &issues);
    if let Err(error) = &terminal {
        let initial_error = format!("{error:#}");
        let mut state = protocol::run_status(database, attempt.id).await;
        if state == "running"
            && let Ok(mut recovered) = protocol::connect(database).await
        {
            terminal = finish(&mut recovered, &attempt, sample, &evidence).await;
            state = protocol::run_status(database, attempt.id).await;
            if state == status {
                *db = recovered;
            }
        }
        event["database_status"] = json!(state);
        event["write_error"] = json!(initial_error);
        if state != status {
            event["status"] = json!("write_failed");
            let saved = preserve(
                attempt.id,
                sample,
                evidence.reply,
                evidence.counters,
                &event,
            );
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
        "EXB2 run_id={} perf session needs recovery: {issues}",
        attempt.id
    );
    Ok(())
}

#[derive(Default)]
struct Acquired {
    reply: Option<ExecBatch>,
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
            match agent.exb2(&protocol::hex(&sample.input)).await {
                Ok(reply) => acquired.reply = Some(reply),
                Err(error) => acquired.issues.push(format!("EXB2 invocation: {error:#}")),
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

fn result_event(
    attempt: &Attempt,
    sample: &Sample,
    status: &str,
    database_status: &str,
    reply: Option<&ExecBatch>,
    issues: &Value,
) -> Value {
    let batch_us = reply.and_then(|reply| reply.end_tick.checked_sub(reply.begin_tick));
    let us_per_round = if status == "complete" {
        batch_us.map(|us| us as f64 / f64::from(ROUNDS))
    } else {
        None
    };
    json!({ "run_id": attempt.id.to_string(), "experiment": "exec_bench", "mode": sample.mode.as_str(),
        "variant": "metrics", "phase": phase(sample.medium_id), "medium_id": sample.medium_id, "retry_of": attempt.retry_of, "input": { "program": sample.program, "n": sample.n, "batch_count": ROUNDS, "seed": SEED },
        "repeat": attempt.repeat, "status": status, "database_status": database_status, "issues": issues,
        "batch_us": batch_us, "us_per_round": us_per_round,
        "execution_return": reply.map(|r| r.execution_return),
    })
}

fn preserve(
    run: Uuid,
    sample: &Sample,
    reply: Option<&ExecBatch>,
    counters: Option<&[u8]>,
    event: &Value,
) -> Result<String> {
    use std::fs;
    let root = std::path::Path::new("exb2-evidence");
    fs::create_dir_all(root)?;
    let path = root.join(run.to_string());
    fs::create_dir(&path)?;
    fs::write(path.join("input.bin"), &sample.input)?;
    fs::write(path.join("expected.bin"), &sample.expected)?;
    if let Some(reply) = reply {
        fs::write(path.join("actual.bin"), &reply.output)?;
    }
    if let Some(counters) = counters {
        fs::write(path.join("metrics.bin"), counters)?;
    }
    fs::write(path.join("attempt.json"), serde_json::to_vec_pretty(event)?)?;
    Ok(path.display().to_string())
}

fn phase(medium_id: Option<Uuid>) -> &'static str {
    if medium_id.is_some() {
        "formal"
    } else {
        "pilot"
    }
}

fn one_pass(pair: &metrics::Pair<'_>) -> bool {
    [
        ("exec_batch_invocations", 1),
        ("exec_batch_action_pass", 1),
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

async fn start(db: &mut Client, attempt: &Attempt, sample: &Sample) -> Result<()> {
    let mode = sample.mode.as_str();
    let condition = Uuid::new_v4();
    let config = attempt.config;
    let tx = db.transaction().await?;
    let parameters = json!({ "workload": sample.program, "n": sample.n, "batch_count": ROUNDS, "seed": SEED, "medium_id": sample.medium_id, "retry_of": attempt.retry_of });
    tx.execute("INSERT INTO bench.condition(condition_id,experiment,protocol_version,parameters,planned_modes) VALUES($1,'exec_bench','1',$2,ARRAY[$3]::text[])", &[&condition,&parameters,&mode]).await?;
    let request = artifact(&tx, "exb2_input", &sample.input).await?;
    let expected = artifact(&tx, "exb2_expected", &sample.expected).await?;
    let digest = protocol::hex(&sample.profile);
    tx.execute(&format!("INSERT INTO {mode}.exec_bench_config(config_id,condition_id,instrumentation,profile_hash,workload,work_param,batch_count,samples_per_run,input_bytes,input_protocol,wifi_enabled,fixture_artifact_id) VALUES($1,$2,'counter_only',$3,$4,$5,$6,1,8258,'EXB2',false,$7)"), &[&config,&condition,&digest,&sample.program,&(sample.n as i32),&i64::from(ROUNDS),&request]).await?;
    tx.execute("INSERT INTO bench.run(run_id,condition_id,config_id,mode,experiment,variant,phase,repeat_no,seed,plan_order,status,run_uuid,libbpf_commit) VALUES($1,$2,$3,$4,'exec_bench','metrics',$5,$6,$7::text::bench.u64,$8,'running',$1,'07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113')", &[&attempt.id,&condition,&config,&mode,&phase(sample.medium_id),&attempt.repeat,&SEED.to_string(),&attempt.order]).await?;
    link(&tx, attempt.id, "exb2_input", request).await?;
    link(&tx, attempt.id, "exb2_expected", expected).await?;
    tx.commit().await?;
    Ok(())
}

struct Completed<'a> {
    reply: Option<&'a ExecBatch>,
    counters: Option<&'a [u8]>,
    pair: Option<metrics::Pair<'a>>,
    status: &'static str,
    issues: &'a Value,
}

async fn finish(
    db: &mut Client,
    attempt: &Attempt,
    sample: &Sample,
    evidence: &Completed<'_>,
) -> Result<()> {
    let mode = sample.mode.as_str();
    let tx = db.transaction().await?;
    let reply = evidence.reply;
    let pair = evidence.pair.as_ref();
    let boot = pair
        .map(|pair| pair.baseline.boot)
        .or_else(|| {
            reply
                .filter(|r| r.boot_id != 0)
                .map(|r| u64::from(r.boot_id))
        })
        .map(|boot| boot.to_string());
    let n = tx.execute("UPDATE bench.run SET status=$2,boot_id=$3::text::bench.u64,issues=$4 WHERE run_id=$1 AND status='running'",
        &[&attempt.id,&evidence.status,&boot,&evidence.issues]).await?;
    ensure!(n == 1, "EXB2 attempt has no running row");
    let snapshots = match pair {
        Some(pair) => Some(pair.write(&tx, mode).await?),
        None => None,
    };
    if let Some(raw) = evidence.counters {
        protocol::artifact(&tx, attempt.id, "metrics_wire", raw).await?;
    }
    let qualified = evidence.status == "complete" && snapshots.is_some();
    let mut completed = 0_i32;
    if let Some(reply) = reply {
        protocol::artifact(&tx, attempt.id, "exb2_actual", &reply.output).await?;
        if let Some(boot) =
            boot.filter(|_| reply.output.len() == BUF && reply.begin_tick <= reply.end_tick)
        {
            let field =
                |offset| u32::from_le_bytes(reply.output[offset..offset + 4].try_into().unwrap());
            let iterations = field(40);
            if iterations <= ROUNDS {
                let result = (u64::from(field(48)) << 32) | u64::from(field(44));
                if snapshots.is_none() {
                    tx.execute("INSERT INTO bench.clock(run_id,clock_id,domain,tick_hz,tick_bits,boot_id,precision_description) VALUES($1,'dut','dut',1000000,64,$2::text::bench.u64,'ESP HAL microseconds since boot')", &[&attempt.id,&boot]).await?;
                }
                tx.execute(&format!("INSERT INTO {mode}.exec_batch(run_id,sample_index,clock_id,begin_tick,end_tick,batch_count,completed_iterations,seed,input_bytes,expected_bytes,actual_bytes,execution_return,status,result,digest,result_ok,qualified) VALUES($1,0,'dut',$2::text::bench.u64,$3::text::bench.u64,$4,$5,$6::text::bench.u64,$7,$8,$9,$10,$11,$12::text::bench.u64,$13,$14,$15)"), &[&attempt.id,&reply.begin_tick.to_string(),&reply.end_tick.to_string(),&i64::from(ROUNDS),&i64::from(iterations),&SEED.to_string(),&sample.input,&sample.expected,&reply.output,&i64::from(reply.execution_return),&i64::from(field(36)),&result.to_string(),&i64::from(field(52)),&verified(sample, reply),&qualified]).await?;
                completed = 1;
            }
        }
    }
    let analysis = Uuid::new_v4();
    let source_digest = protocol::digest(
        evidence
            .counters
            .or(reply.map(|reply| reply.output.as_slice()))
            .unwrap_or(&sample.input),
    );
    tx.execute("INSERT INTO bench.analysis(analysis_id,run_id,algorithm_version,source_digest,qualified,issues) VALUES($1,$2,'exb2-equality/nms1-v1',$3,$4,$5)",
        &[&analysis,&attempt.id,&source_digest,&qualified,&evidence.issues]).await?;
    let baseline = snapshots.map(|(start, _)| start);
    let final_ = snapshots.map(|(_, end)| end);
    tx.execute(&format!("INSERT INTO {mode}.exec_bench_metrics_data(run_id,config_id,baseline_snapshot_id,final_snapshot_id,analysis_id,qualified,issues,batches_expected,batches_completed,valid_batches) VALUES($1,$2,$3,$4,$5,$6,$7,1,$8,$9)"),
        &[&attempt.id,&attempt.config,&baseline,&final_,&analysis,&qualified,&evidence.issues,&completed,&i32::from(qualified)]).await?;
    tx.commit().await?;
    Ok(())
}

async fn artifact(tx: &Transaction<'_>, role: &str, bytes: &[u8]) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let hash = protocol::digest(bytes);
    tx.execute("INSERT INTO bench.artifact(artifact_id,role,sha256,size_bytes,original_bytes,media_type) VALUES($1,$2,$3,$4,$5,'application/octet-stream')", &[&id,&role,&hash,&(bytes.len() as i64),&bytes]).await?;
    Ok(id)
}

async fn link(tx: &Transaction<'_>, run: Uuid, role: &str, id: Uuid) -> Result<()> {
    tx.execute(
        "INSERT INTO bench.run_artifact(run_id,role,artifact_id) VALUES($1,$2,$3)",
        &[&run, &role, &id],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires a disposable protocol database XPOST_CASE_TEST_PG"]
    async fn exb2_attempt_roundtrips_in_all_four_protocol_schemas() -> Result<()> {
        let dsn = std::env::var("XPOST_CASE_TEST_PG")?;
        let (mut client, session) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls).await?;
        tokio::spawn(session);
        for mode in [Mode::Hal, Mode::Aot, Mode::Jit, Mode::Rbpf] {
            let (n, input, expected) = exb2::request(0);
            let sample = Sample {
                mode,
                program: "noop",
                n,
                input,
                expected,
                profile: [0x2a; 32],
                medium_id: None,
            };
            let attempt = Attempt {
                id: Uuid::new_v4(),
                order: 0,
                repeat: 1,
                config: Uuid::new_v4(),
                retry_of: None,
            };
            start(&mut client, &attempt, &sample).await?;
            let response = ExecBatch {
                boot_id: 11,
                begin_tick: 110,
                end_tick: 125,
                execution_return: if mode == Mode::Hal { 0 } else { 2 },
                output: sample.expected.clone(),
            };
            assert!(verified(&sample, &response));
            let raw = [
                synthetic(&attempt, &sample, 1, 100, 7),
                synthetic(&attempt, &sample, 2, 150, 8),
            ]
            .concat();
            let pair = metrics::decode(&raw)?;
            pair.require(attempt.id, 3, 11, &sample.profile, 100)?;
            assert!(one_pass(&pair));
            let issues = json!([]);
            finish(
                &mut client,
                &attempt,
                &sample,
                &Completed {
                    reply: Some(&response),
                    counters: Some(&raw),
                    pair: Some(pair),
                    status: "complete",
                    issues: &issues,
                },
            )
            .await?;
            let row = client.query_one(&format!("SELECT r.status, e.actual_bytes, e.result_ok, e.qualified FROM bench.run r JOIN {}.exec_batch e USING (run_id) WHERE r.run_id=$1", mode.as_str()), &[&attempt.id]).await?;
            assert_eq!(row.get::<_, String>(0), "complete");
            assert_eq!(row.get::<_, Vec<u8>>(1), sample.expected);
            assert!(row.get::<_, bool>(2));
            assert!(row.get::<_, bool>(3));
            let metrics = client
                .query_one(
                    &format!(
                        "SELECT count(*) FROM {}.metric_snapshot WHERE run_id=$1",
                        mode.as_str()
                    ),
                    &[&attempt.id],
                )
                .await?;
            assert_eq!(metrics.get::<_, i64>(0), 2);
            eprintln!("EXB2 {} fixture run_id={}", mode.as_str(), attempt.id);
        }
        Ok(())
    }

    fn synthetic(attempt: &Attempt, sample: &Sample, phase: u8, tick: u64, count: u64) -> Vec<u8> {
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
        frame.extend(attempt.id.as_bytes());
        frame.extend(3_u32.to_le_bytes());
        frame.extend(11_u64.to_le_bytes());
        frame.extend(sample.profile);
        frame.extend(tick.to_le_bytes());
        frame.extend(1_000_000_u32.to_le_bytes());
        frame.extend(pfr);
        frame
    }
}
