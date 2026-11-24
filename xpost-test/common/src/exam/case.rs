//! Case (AOT Trace).
//!
//! 1. Verify both XDP ELFs. Open the protocol database and register the run ID.
//! 2. Flash one firmware and cold-install both images. Warm both SRAM slots.
//! 3. Check tagged CAS2 replies from each slot. Restore A as active and B as staged.
//! 4. Start tcpdump and send JTX1 probes. Arm a Trace epoch after stable A output.
//! 5. Publish B once. Observe valid B replies and retain a bounded capture tail.
//! 6. Freeze and export Trace. Save raw pcap, Trace, and switch status under one run ID.
//!
//! Packet time, runner command time, Agent reception time, and DUT ticks have separate clocks.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use uuid::Uuid;

use crate::agent::AgentClient;
use crate::config::FileConfig;
use crate::pipeline::{READY_BUDGET, RoundUi, status};

use super::{
    case_capture::{Capture, POST, PRE},
    case_store::{self, Attempt, Observation, Switch},
    case_trace,
};

const FIRMWARE: &str = "case-xdp";
const SLOT_A: &str = "case_swap";
const SLOT_B: &str = "case_mark_idle";

pub async fn run_case(cfg: &FileConfig, phase: &str) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("case needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    agent.health().await.context("case Agent health")?;
    let digest_a = agent
        .preflight_xdp(SLOT_A)
        .await
        .context("prepare Slot A")?;
    let digest_b = agent
        .preflight_xdp(SLOT_B)
        .await
        .context("prepare Slot B")?;
    let mut client = case_store::connect(&cfg.postgres).await?;
    let run = Uuid::new_v4();
    let profile = case_store::hex(&Sha256::digest(b"case-aot-trace-v0.1.1"));
    let attempt = Attempt::start(&mut client, run, phase, &profile).await?;
    eprintln!("case AOT Trace run_id={run} phase={phase}");
    let mut observation = Observation::default();
    if let Err(error) = perform(
        cfg,
        &agent,
        run,
        &profile,
        [&digest_a, &digest_b],
        &mut observation,
    )
    .await
    {
        observation.issues.push(format!("{error:#}"));
    }
    if let Err(error) = attempt
        .finish(&mut client, &observation, &cfg.probe.iface)
        .await
    {
        let saved = observation
            .preserve(run)
            .map(|path| path.display().to_string());
        return Err(error).with_context(|| match saved {
            Ok(path) => {
                format!("case run_id={run} database write failed. Evidence saved at {path}")
            }
            Err(save) => format!(
                "case run_id={run} database write failed. Local evidence export failed: {save:#}"
            ),
        });
    }
    ensure!(
        observation.qualified(),
        "case run_id={run}: {}",
        observation.issues.join("; ")
    );
    eprintln!("case AOT Trace complete run_id={run} switch_index=1 direction=A→B");
    Ok(())
}

async fn perform(
    cfg: &FileConfig,
    agent: &AgentClient,
    run: Uuid,
    profile: &str,
    digests: [&str; 2],
    result: &mut Observation,
) -> Result<()> {
    let ui = RoundUi::new();
    ui.begin_round();
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    let identity = agent.flash_fw_evidence(FIRMWARE).await?;
    let checked = check_flashed(&identity, digests);
    result.flash = Some(identity);
    result
        .preparation
        .push("firmware flash; cold install case_swap and case_mark_idle".into());
    checked?;
    status::settle_checklist(&ui, agent, READY_BUDGET).await?;
    agent.disable_xdp().await?;
    let gen_a = stage(agent, SLOT_A, 0).await?;
    let gen_b = stage(agent, SLOT_B, 1).await?;
    result.preparation.push(format!(
        "preload A generation {gen_a}; preload B generation {gen_b}"
    ));
    agent.enable_xdp().await?;
    let initial = board(agent).await?;
    result.boot = Some(u64::from(initial.boot));
    initial.require(0, 1, gen_b, initial.boot)?;
    let wrong_generation = if gen_b == u32::MAX { 1 } else { gen_b + 1 };
    let rejected = agent.switch_xdp(1, wrong_generation, "drop").await;
    ensure!(
        rejected.is_err(),
        "DUT accepted a publish with the wrong generation"
    );
    let unchanged = board(agent).await?;
    unchanged.require(0, 1, gen_b, initial.boot)?;
    ensure!(
        unchanged.last_error == 4,
        "DUT did not report bad-generation after rejected publish"
    );
    result
        .preparation
        .push("rejected wrong B generation; A stayed active".into());

    // Qualification uses separate tags so its B probes cannot enter the formal window.
    let qualification = Capture::start(cfg, Uuid::new_v4())?;
    qualification.observe(2).await?;
    let b = agent.switch_xdp(1, gen_b, "drop").await?;
    check_published(&b, 0, 1, gen_b)?;
    qualification.observe(1).await?;
    board(agent).await?.require(1, 0, gen_a, initial.boot)?;
    let a = agent.switch_xdp(0, gen_a, "drop").await?;
    check_published(&a, 1, 0, gen_a)?;
    qualification.stop().await?;
    result
        .preparation
        .push(format!("qualify and restore A: {b}; {a}"));
    board(agent).await?.require(0, 1, gen_b, initial.boot)?;

    let capture_start = Instant::now();
    let capture = Capture::start(cfg, run)?;
    capture.observe(2).await?;
    tokio::time::sleep(PRE).await;
    let (epoch, boot) = agent.trace_prepare(run, profile).await?;
    result
        .preparation
        .push(format!("Trace epoch {epoch} boot {boot}"));
    ensure!(
        boot == u64::from(initial.boot),
        "DUT boot changed before Trace prepare"
    );
    agent.trace_arm(epoch).await?;
    let begin_ns = capture_start.elapsed().as_nanos();
    let reply = tokio::time::timeout(Duration::from_secs(5), agent.switch_xdp(1, gen_b, "drop"))
        .await
        .context("switch response deadline")
        .and_then(|result| result);
    let end_ns = capture_start.elapsed().as_nanos();
    let command = reply
        .as_ref()
        .map_err(|error| anyhow::anyhow!("publication outcome unknown: {error:#}"))
        .and_then(|reply| check_published(reply, 0, 1, gen_b));
    let observed = capture.observe(1).await;
    if observed.is_ok() {
        tokio::time::sleep(POST).await;
    }
    let captured = capture.stop().await;
    let frozen = agent.trace_freeze_read(epoch).await;
    let released = if frozen.is_ok() {
        agent.trace_release(epoch).await
    } else {
        Err(anyhow::anyhow!(
            "Trace export failed for epoch {epoch}. Check this epoch before a new publish"
        ))
    };
    let state = board(agent).await;
    let boot_unchanged = state.as_ref().is_ok_and(|now| now.boot == initial.boot);
    let confirmed = state
        .as_ref()
        .map_err(|e| anyhow::anyhow!("read post-publish status: {e:#}"))
        .and_then(|now| now.require(1, 0, gen_a, initial.boot));
    result.switch = Some(Switch {
        generation_a: gen_a,
        generation_b: gen_b,
        begin_ns,
        end_ns,
        command_ok: command.is_ok(),
        boot_unchanged,
        reply: reply.ok(),
        state_after: state.ok().map(|s| s.raw),
        error: command.as_ref().err().map(|error| format!("{error:#}")),
    });
    for check in [command, observed, confirmed, released] {
        if let Err(error) = check {
            result.issues.push(format!("{error:#}"));
        }
    }
    match captured {
        Ok(data) => result.capture = Some(data),
        Err(error) => result.issues.push(format!("tcpdump export: {error:#}")),
    }
    if let Some(capture) = &result.capture {
        if !capture.loss_free() {
            result
                .issues
                .push("tcpdump kernel loss is nonzero or unknown".into());
        }
        let (a, b, _) = result.transitions();
        if a.is_none() || b.is_none() {
            result
                .issues
                .push("capture lacks a valid A→B reply transition".into());
        }
    }
    match frozen {
        Ok(raw) => {
            let trace = case_trace::decode(&raw, run, epoch, boot);
            result.trace_bytes = Some(raw);
            match trace {
                Ok(trace) => {
                    if !trace.proves_publish(gen_b) {
                        result
                            .issues
                            .push("DUT Trace lacks the expected B publish boundaries".into());
                    }
                    result.trace = Some(trace);
                }
                Err(error) => result.issues.push(format!("Trace validation: {error:#}")),
            }
        }
        Err(error) => result.issues.push(format!("Trace export: {error:#}")),
    }
    Ok(())
}

fn check_flashed(identity: &serde_json::Value, digests: [&str; 2]) -> Result<()> {
    let firmware_hash = identity["firmware_sha256"]
        .as_str()
        .context("missing Firmware ELF digest")?;
    ensure!(
        identity["firmware_profile"] == FIRMWARE,
        "Agent returned a different firmware profile"
    );
    let containers = identity["containers"]
        .as_array()
        .context("missing installed images")?;
    ensure!(
        containers.len() == 2,
        "case requires two installed XDP ELFs"
    );
    for (slot, (name, digest)) in [SLOT_A, SLOT_B].into_iter().zip(digests).enumerate() {
        ensure!(
            containers.iter().any(|image| image["profile"] == name
                && image["slot"].as_u64() == Some(slot as u64)
                && image["mode"] == "aot"
                && image["firmware_sha256"] == firmware_hash
                && image["container_sha256"] == digest),
            "installed case image {name} differs from preflight"
        );
    }
    Ok(())
}

async fn stage(agent: &AgentClient, name: &str, slot: u8) -> Result<u32> {
    let reply = agent.flash_xdp_slot(name, slot).await?;
    let mut fields = reply
        .strip_prefix("OK xdp flash ")
        .context("invalid preload reply")?
        .split_whitespace();
    ensure!(
        fields
            .next()
            .context("missing preload slot")?
            .parse::<u8>()?
            == slot,
        "preload acknowledged another slot"
    );
    let generation = fields
        .next()
        .context("missing preload generation")?
        .parse()?;
    ensure!(generation != 0, "preload returned zero generation");
    Ok(generation)
}

fn check_published(reply: &str, from: u8, to: u8, generation: u32) -> Result<()> {
    let expected = format!("OK xdp switch 1 0 0 {from} {to} {generation} ");
    ensure!(
        reply.starts_with(&expected),
        "unexpected publish result {reply}"
    );
    Ok(())
}

struct Status {
    boot: u32,
    active: u32,
    staged: u32,
    staged_generation: u32,
    updating: u32,
    severe: u32,
    last_error: u32,
    raw: String,
}

impl Status {
    fn require(&self, active: u32, staged: u32, generation: u32, boot: u32) -> Result<()> {
        ensure!(
            self.boot != 0
                && self.boot == boot
                && self.active == active
                && self.staged == staged
                && self.staged_generation == generation
                && self.updating == 0
                && self.severe == 0,
            "DUT slot state differs: {}",
            self.raw
        );
        Ok(())
    }
}

async fn board(agent: &AgentClient) -> Result<Status> {
    let raw = agent.xdp_status().await?;
    let fields = raw
        .strip_prefix("OK get xdp ")
        .context("invalid slot status reply")?
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()?;
    let [
        boot,
        active,
        staged,
        staged_generation,
        updating,
        severe,
        _,
        _,
        last_error,
    ] = fields.as_slice()
    else {
        anyhow::bail!("invalid slot status length");
    };
    Ok(Status {
        boot: *boot,
        active: *active,
        staged: *staged,
        staged_generation: *staged_generation,
        updating: *updating,
        severe: *severe,
        last_error: *last_error,
        raw,
    })
}
