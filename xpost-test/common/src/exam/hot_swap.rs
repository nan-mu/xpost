//! Quiescent A/B swap on `case_xdp`.
//!
//! Cold install (`clear` / `flash_xdp`) may reboot. After `OK init`, stage and
//! activate must keep the same boot id. `case_swap` is linked at slot 0.
//! `case_mark_idle` is the same `case_mark` program linked at `xdp_slot_idle`.
//! Returning to the first image is another stage plus activate, chosen by the
//! host. This exam does not touch `rxcnt`.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::agent::AgentClient;
use crate::config::{FileConfig, format_mac};
use crate::db::{self, CaseObs, PgOpts};
use crate::exam::case::{self, MARK_DST, PAYLOAD_LEN, PORT, SENT, STIM_SRC};
use crate::pipeline::{BudgetEta, READY_BUDGET, RoundUi, status};

const FW: &str = "case-cag";
const SWAP: &str = "case_swap";
const MARK: &str = "case_mark_idle";

#[allow(dead_code)]
struct Wire {
    boot_id: u32,
    active: u8,
    staged: u8,
    generation: u32,
    updating: u8,
    severe: u8,
    drop: u32,
    pass: u32,
    err: u8,
}

pub async fn run_hot_swap(cfg: &FileConfig) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("hot_swap needs [agent].endpoint")?;
    let agent = AgentClient::new(endpoint);
    let client = db::connect_and_ensure(PgOpts { formal: true }).await?;
    let exam_id = db::insert_exam(&client, Some("hot_swap")).await?;
    eprintln!("hot_swap exam={exam_id} firmware={FW}");

    let ui = RoundUi::new();
    let mut eta = BudgetEta::new();
    eta.add_flashes(1);
    ui.begin_round();
    ui.check_pending(&format!("烧录 `{FW}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.check_pending("DUT ready");
    status::flash_idle(
        &ui,
        &agent,
        FW,
        std::time::Duration::from_secs(300),
        &mut eta,
    )
    .await?;
    status::settle_checklist(&ui, &agent, READY_BUDGET).await?;

    agent.clear_xdp("*").await.context("clear cold table")?;
    agent.flash_xdp(SWAP).await.context("install case_swap")?;
    agent
        .flash_xdp(MARK)
        .await
        .context("install case_mark_idle")?;
    agent
        .get_init()
        .await
        .context("OK init after both images")?;

    let mac = format_mac(&cfg.esp_mac_bytes()?);
    wait_dut(cfg, &mac).await?;
    let swap_needle = format!(
        "{}.{} > {}.{}",
        cfg.target.esp_ip, PORT, STIM_SRC, cfg.target.stim_sport
    );

    let boot = status_of(&agent).await?;
    if boot.boot_id == 0 || boot.active != 0 {
        bail!("boot status want active slot 0, got {}", boot.raw);
    }
    observe(
        cfg,
        &agent,
        &client,
        exam_id,
        1,
        SWAP,
        &mac,
        &swap_needle,
        &boot.raw,
    )
    .await?;

    let bad_addr = agent.flash_xdp_slot(SWAP, 1).await;
    if bad_addr.is_ok() {
        bail!("stage case_swap into slot 1 should fail: wrong link address");
    }
    let after_bad = status_of(&agent).await?;
    if after_bad.boot_id != boot.boot_id || after_bad.active != 0 {
        bail!("bad stage changed the lease: {}", after_bad.raw);
    }
    observe(
        cfg,
        &agent,
        &client,
        exam_id,
        2,
        SWAP,
        &mac,
        &swap_needle,
        &after_bad.raw,
    )
    .await?;

    let staged = agent
        .flash_xdp_slot(MARK, 1)
        .await
        .context("copy case_mark_idle into slot 1")?;
    let generation = parse_stage(&staged)?;
    let staged_status = status_of(&agent).await?;
    if staged_status.boot_id != boot.boot_id
        || staged_status.active != 0
        || staged_status.staged != 1
    {
        bail!("stage published or rebooted: {}", staged_status.raw);
    }
    observe(
        cfg,
        &agent,
        &client,
        exam_id,
        3,
        SWAP,
        &mac,
        &swap_needle,
        &format!("{staged} {}", staged_status.raw),
    )
    .await?;

    let bad_gen = agent
        .activate_xdp(1, generation.wrapping_add(1), "drop")
        .await;
    if bad_gen.is_ok() {
        bail!("activate with a stale generation should fail");
    }
    let still = status_of(&agent).await?;
    if still.boot_id != boot.boot_id || still.active != 0 {
        bail!("bad generation changed the lease: {}", still.raw);
    }
    observe(
        cfg,
        &agent,
        &client,
        exam_id,
        4,
        SWAP,
        &mac,
        &swap_needle,
        &still.raw,
    )
    .await?;

    let activated = agent
        .activate_xdp(1, generation, "drop")
        .await
        .context("activate case_mark_idle")?;
    let now = status_of(&agent).await?;
    if now.boot_id != boot.boot_id || now.active != 1 || now.severe != 0 {
        bail!("activate did not publish slot 1: {} {activated}", now.raw);
    }
    observe(
        cfg,
        &agent,
        &client,
        exam_id,
        5,
        MARK,
        &mac,
        MARK_DST,
        &format!("{activated} {}", now.raw),
    )
    .await?;

    let restaged = agent
        .flash_xdp_slot(SWAP, 0)
        .await
        .context("copy previous image into the idle slot")?;
    let back_gen = parse_stage(&restaged)?;
    let rolled = agent
        .activate_xdp(0, back_gen, "drop")
        .await
        .context("publish previous image")?;
    let back = status_of(&agent).await?;
    if back.boot_id != boot.boot_id || back.active != 0 || back.severe != 0 {
        bail!("republish did not restore slot 0: {} {rolled}", back.raw);
    }
    observe(
        cfg,
        &agent,
        &client,
        exam_id,
        6,
        SWAP,
        &mac,
        &swap_needle,
        &format!("{rolled} {}", back.raw),
    )
    .await?;

    db::stop_exam(&client, exam_id).await?;
    eprintln!("hot_swap ok exam={exam_id} boot={}", boot.boot_id);
    Ok(())
}

struct Status {
    boot_id: u32,
    active: u8,
    staged: u8,
    severe: u8,
    raw: String,
}

async fn wait_dut(cfg: &FileConfig, mac: &str) -> Result<()> {
    let needle = format!("{mac} >");
    for i in 0..40 {
        let dump =
            case::sniff(cfg, mac, &[b"WARM!"], std::time::Duration::from_millis(400)).await?;
        if dump
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
        {
            eprintln!("hot_swap link up");
            return Ok(());
        }
        if i % 5 == 4 {
            eprintln!("hot_swap still waiting for a DUT frame");
        }
    }
    bail!("hot_swap saw no DUT frame during warmup")
}

async fn status_of(agent: &AgentClient) -> Result<Status> {
    let raw = agent.xdp_status().await.context("get xdp")?;
    let wire = parse_status(&raw)?;
    Ok(Status {
        boot_id: wire.boot_id,
        active: wire.active,
        staged: wire.staged,
        severe: wire.severe,
        raw,
    })
}

async fn observe(
    cfg: &FileConfig,
    _agent: &AgentClient,
    client: &tokio_postgres::Client,
    exam_id: db::ExamId,
    seq: i32,
    xdp: &str,
    mac: &str,
    needle: &str,
    note: &str,
) -> Result<()> {
    let scored: Vec<[u8; PAYLOAD_LEN]> = (0..SENT).map(case::payload).collect();
    let refs: Vec<&[u8]> = scored.iter().map(|p| p.as_slice()).collect();
    let dump = case::sniff(cfg, mac, &refs, std::time::Duration::from_millis(800)).await?;
    let hits = case::payload_hits(&dump, mac, needle);
    let matched = hits.iter().filter(|hit| **hit).count();
    let ok = matched == SENT;
    let mut text = format!("# {note}\n# host_ms {}\n", host_ms());
    text.push_str(&dump);
    db::insert_case_obs(
        client,
        exam_id,
        &CaseObs {
            seq,
            xdp,
            capability: "quiescent hot swap",
            action: "XDP_TX",
            expect: "DUT-sourced UDP length 5 carries the needle for the image that should be active. Boot id is unchanged.",
            map_name: "",
            map_slot: 0,
            map_before: 0,
            map_after: 0,
            sent: SENT as i32,
            saw_reply: ok,
            ok,
            tcpdump: &text,
        },
    )
    .await?;
    if !ok {
        bail!("hot_swap seq={seq} {xdp} payloads {matched}/{SENT} needle={needle}");
    }
    eprintln!("hot_swap seq={seq} {xdp} ok needle={needle}");
    Ok(())
}

fn parse_stage(msg: &str) -> Result<u32> {
    let rest = msg
        .trim()
        .strip_prefix("OK xdp stage ")
        .context("stage reply")?;
    let mut parts = rest.split_whitespace();
    let _slot: u8 = parts
        .next()
        .context("stage slot")?
        .parse()
        .context("stage slot")?;
    let generation = parts
        .next()
        .context("stage generation")?
        .parse()
        .context("generation")?;
    Ok(generation)
}

fn parse_status(msg: &str) -> Result<Wire> {
    let rest = msg
        .trim()
        .strip_prefix("OK get xdp ")
        .with_context(|| format!("status reply: {msg}"))?;
    let nums: Vec<u32> = rest
        .split_whitespace()
        .map(|s| s.parse().with_context(|| format!("status number {s}")))
        .collect::<Result<_>>()?;
    if nums.len() != 9 {
        bail!("status wants 9 numbers, got {msg}");
    }
    Ok(Wire {
        boot_id: nums[0],
        active: nums[1] as u8,
        staged: nums[2] as u8,
        generation: nums[3],
        updating: nums[4] as u8,
        severe: nums[5] as u8,
        drop: nums[6],
        pass: nums[7],
        err: nums[8] as u8,
    })
}

fn host_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
