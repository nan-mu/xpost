//! Quiescent A/B publish on `case_xdp`.
//!
//! Cold install (`clear` / `flash_xdp`) may reboot and is not timed. After both
//! images are in the cold table, the exam drops the lease, copies each image
//! into its slot, and leases slot 0. The timed command is `switch` to slot 1.
//! `t0` is that command. `t1` is the first captured slot-B frame at or after
//! `t0`. Slot fill is outside the window. This exam does not touch `rxcnt`.

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

    agent
        .disable_xdp()
        .await
        .context("drop the lease before filling slots")?;
    let filled_a = agent
        .flash_xdp_slot(SWAP, 0)
        .await
        .context("copy case_swap into slot 0")?;
    let _slot_a = parse_flash(&filled_a)?;
    let filled_b = agent
        .flash_xdp_slot(MARK, 1)
        .await
        .context("copy case_mark_idle into slot 1")?;
    let generation = parse_flash(&filled_b)?;
    agent.enable_xdp().await.context("lease slot 0")?;

    let boot = status_of(&agent).await?;
    if boot.boot_id == 0 || boot.active != 0 {
        bail!("enable should run slot 0, got {}", boot.raw);
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
        &format!("{filled_a} {filled_b} {}", boot.raw),
    )
    .await?;

    let boot_id = boot.boot_id;
    let published = case::capture_update(
        cfg,
        &mac,
        &swap_needle,
        MARK_DST,
        case::CaptureWindow {
            interval: std::time::Duration::from_millis(2),
            preamble: std::time::Duration::from_millis(200),
            tail: std::time::Duration::from_millis(500),
        },
        || async {
            let t0 = case::unix_secs();
            let switched = agent
                .switch_xdp(1, generation, "drop")
                .await
                .context("switch to slot 1")?;
            let now = status_of(&agent).await?;
            if now.boot_id != boot_id || now.active != 1 || now.severe != 0 {
                bail!("switch did not publish slot 1: {} {switched}", now.raw);
            }
            eprintln!("hot_swap switch {switched} {}", now.raw);
            Ok(t0)
        },
    )
    .await?;
    if published.old_before == 0 {
        bail!(
            "hot_swap preamble did not see slot A:\n{}",
            published.report
        );
    }
    eprintln!("hot_swap switch->mark {}", published.summary);
    store_gap(&client, exam_id, 2, MARK, &published).await?;

    db::stop_exam(&client, exam_id).await?;
    eprintln!(
        "hot_swap ok exam={exam_id} boot={} command_ms={}",
        boot.boot_id,
        published
            .command_ms
            .map(|ms| format!("{ms:.3}"))
            .unwrap_or_else(|| "none".to_string())
    );
    Ok(())
}

struct Status {
    boot_id: u32,
    active: u8,
    severe: u8,
    raw: String,
}

async fn store_gap(
    client: &tokio_postgres::Client,
    exam_id: db::ExamId,
    seq: i32,
    xdp: &str,
    gap: &case::WireGap,
) -> Result<()> {
    if gap.command_ms.is_none() {
        bail!("hot_swap seq={seq} capture has no frame at or after switch:\n{}", gap.report);
    }
    db::insert_case_obs(
        client,
        exam_id,
        &CaseObs {
            seq,
            xdp,
            capability: "quiescent hot swap",
            action: "XDP_TX",
            expect: "t0 is the host clock when switch is issued. t1 is the first slot-B DUT frame with tcpdump -tt >= t0. The difference includes HTTP, UART, publish, and one air hop. Slot fill is outside this window.",
            map_name: "",
            map_slot: 0,
            map_before: 0,
            map_after: 0,
            sent: SENT as i32,
            saw_reply: gap.new_replies > 0,
            ok: gap.command_ms.is_some() && gap.old_before > 0 && gap.new_replies > 0,
            tcpdump: &gap.report,
        },
    )
    .await?;
    Ok(())
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

fn parse_flash(msg: &str) -> Result<u32> {
    let rest = msg
        .trim()
        .strip_prefix("OK xdp flash ")
        .context("flash reply")?;
    let mut parts = rest.split_whitespace();
    let _slot: u8 = parts
        .next()
        .context("flash slot")?
        .parse()
        .context("flash slot")?;
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
