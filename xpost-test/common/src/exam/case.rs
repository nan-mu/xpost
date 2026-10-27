//! One firmware, two cold images, tcpdump text in Postgres.
//!
//! `case_tx` is `just_tx.c`. UDP increments `rxcnt[17]` and returns `XDP_TX`
//! after an Ethernet MAC swap. `case_drop` is `just_drop.c`. IPv4 increments
//! `counter[src_last_octet]` and returns `XDP_DROP`.
//!
//! The firmware ELF is flashed once. The second step calls `disable xdp`,
//! clears the cold table, and installs the other image. tcpdump writes no
//! pcap. Its stdout and stderr stay in the `case_obs.tcpdump` column.

use std::net::Ipv4Addr;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::agent::AgentClient;
use crate::config::{FileConfig, format_mac};
use crate::db::{self, CaseObs, PgOpts};
use crate::pipeline::{self, BudgetEta, READY_BUDGET, RoundUi, status};

const FW: &str = "case-cag";
const PORT: u16 = 8080;
const SENT: u64 = 4;
const IPPROTO_UDP: u32 = 17;
const STIM_SRC: Ipv4Addr = Ipv4Addr::new(192, 168, 110, 50);
const DROP_SLOT: u32 = 50;
const READY_TRIES: u32 = 40;
const DUMP_CAP: usize = 32 * 1024;

struct Step {
    seq: i32,
    xdp: &'static str,
    map: &'static str,
    slot: u32,
    action: &'static str,
    expect_reply: bool,
    capability: &'static str,
    expect: &'static str,
    replace: bool,
}

const STEPS: &[Step] = &[
    Step {
        seq: 1,
        xdp: "case_tx",
        map: "rxcnt",
        slot: IPPROTO_UDP,
        action: "XDP_TX",
        expect_reply: true,
        capability: "count, reply",
        expect: "UDP to port 8080 increments rxcnt[17] by the send count, swaps Ethernet MACs, and returns XDP_TX. tcpdump shows the DUT MAC as an Ethernet source. Non-UDP would increment another rxcnt slot and return XDP_DROP.",
        replace: false,
    },
    Step {
        seq: 2,
        xdp: "case_drop",
        map: "counter",
        slot: DROP_SLOT,
        action: "XDP_DROP",
        expect_reply: false,
        capability: "count, filter",
        expect: "IPv4 from 192.168.110.50 increments counter[50] by the send count and returns XDP_DROP. No header field changes. tcpdump does not show the DUT MAC as an Ethernet source.",
        replace: true,
    },
];

pub async fn run_case(cfg: &FileConfig) -> Result<()> {
    let endpoint = cfg
        .agent
        .resolved_endpoint()
        .context("case needs [agent].endpoint in config/default.toml")?;
    let agent = AgentClient::new(endpoint);
    let pg_opts = PgOpts { formal: true };
    let client = db::connect_and_ensure(pg_opts).await?;
    let exam_id = db::insert_exam(&client, Some("case")).await?;
    eprintln!("case exam={exam_id} firmware={FW}");

    let ui = RoundUi::new();
    let mut eta = BudgetEta::new();
    eta.add_flashes(1);
    ui.begin_round();
    ui.check_pending(&format!("烧录 `{FW}`"));
    ui.check_pending(&format!("settle ≤{}s", READY_BUDGET.as_secs()));
    ui.check_pending("DUT ready");
    status::flash_idle(&ui, &agent, FW, Duration::from_secs(300), &mut eta).await?;
    status::settle_checklist(&ui, &agent, READY_BUDGET).await?;

    for step in STEPS {
        run_step(cfg, &agent, &client, exam_id, step).await?;
    }
    db::stop_exam(&client, exam_id).await?;
    eprintln!("case ok exam={exam_id}");
    Ok(())
}

async fn run_step(
    cfg: &FileConfig,
    agent: &AgentClient,
    client: &tokio_postgres::Client,
    exam_id: db::ExamId,
    step: &Step,
) -> Result<()> {
    if step.replace {
        agent
            .disable_xdp()
            .await
            .with_context(|| format!("disable xdp before {}", step.xdp))?;
        eprintln!("case seq={} disabled previous image", step.seq);
    }
    agent
        .clear_xdp("*")
        .await
        .with_context(|| format!("clear xdp before {}", step.xdp))?;
    agent
        .flash_xdp(step.xdp)
        .await
        .with_context(|| format!("flash xdp {}", step.xdp))?;
    agent
        .get_init()
        .await
        .with_context(|| format!("OK init after {}", step.xdp))?;
    agent
        .enable_xdp()
        .await
        .with_context(|| format!("enable xdp {}", step.xdp))?;
    wait_map(cfg, agent, step).await?;
    agent
        .clear_map(step.map)
        .await
        .with_context(|| format!("clear map {}", step.map))?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let before = agent.map_slot(step.map, step.slot).await?;
    let mac = format_mac(&cfg.esp_mac_bytes()?);
    let dump = capture(cfg, &mac).await?;
    let after = agent.map_slot(step.map, step.slot).await?;
    let saw_reply = dump.to_ascii_lowercase().contains(&format!("{mac} >"));
    let replies = dump
        .lines()
        .filter(|line| line.to_ascii_lowercase().contains(&format!("{mac} >")))
        .count() as u64;
    let delta = after.saturating_sub(before);
    // rxcnt counts every UDP frame the radio delivers, including retries.
    let ok = delta >= SENT && (replies > 0) == step.expect_reply;
    eprintln!(
        "case seq={} xdp={} ok={ok} action={} {}[{}] {before}->{after} sent={SENT} saw_reply={saw_reply}",
        step.seq, step.xdp, step.action, step.map, step.slot
    );
    let excerpt: String = dump.lines().take(12).collect::<Vec<_>>().join("\n");
    eprintln!("case tcpdump excerpt:\n{excerpt}");

    db::insert_case_obs(
        client,
        exam_id,
        &CaseObs {
            seq: step.seq,
            xdp: step.xdp,
            capability: step.capability,
            action: step.action,
            expect: step.expect,
            map_name: step.map,
            map_slot: step.slot as i32,
            map_before: before as i64,
            map_after: after as i64,
            sent: SENT as i32,
            saw_reply,
            ok,
            tcpdump: &dump,
        },
    )
    .await?;
    if !ok {
        bail!(
            "case {} mismatch: map {before}->{after} (want at least +{SENT}), replies={replies} want_reply={}",
            step.xdp,
            step.expect_reply
        );
    }
    Ok(())
}

async fn wait_map(cfg: &FileConfig, agent: &AgentClient, step: &Step) -> Result<()> {
    for i in 0..READY_TRIES {
        let before = agent.map_slot(step.map, step.slot).await?;
        burst(cfg, 2).await?;
        tokio::time::sleep(Duration::from_millis(400)).await;
        let after = agent.map_slot(step.map, step.slot).await?;
        if after > before {
            eprintln!(
                "case {} link up {}[{}] {before}->{after}",
                step.xdp, step.map, step.slot
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
            return Ok(());
        }
        if i % 5 == 4 {
            eprintln!("case {} still waiting for {}", step.xdp, step.map);
        }
    }
    bail!("case {} map {} did not move", step.xdp, step.map);
}

async fn capture(cfg: &FileConfig, mac: &str) -> Result<String> {
    let mut child = Command::new("tcpdump")
        .args([
            "-e",
            "-n",
            "-l",
            "-tt",
            "-i",
            &cfg.probe.iface,
            "udp",
            "and",
            "port",
            "8080",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn tcpdump")?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    burst(cfg, SENT).await?;
    tokio::time::sleep(Duration::from_millis(800)).await;
    child.start_kill().ok();
    let mut stdout = child.stdout.take().context("tcpdump stdout")?;
    let mut stderr = child.stderr.take().context("tcpdump stderr")?;
    let mut out = String::new();
    let mut err = String::new();
    let _ = child.wait().await;
    stdout.read_to_string(&mut out).await.context("read tcpdump stdout")?;
    stderr.read_to_string(&mut err).await.context("read tcpdump stderr")?;
    let mut text = format!("# dut_mac {mac}\n{out}");
    if !err.is_empty() {
        text.push_str("\n# stderr\n");
        text.push_str(&err);
    }
    if text.len() > DUMP_CAP {
        text.truncate(DUMP_CAP);
        text.push_str("\n[truncated]\n");
    }
    Ok(text)
}

async fn burst(cfg: &FileConfig, n: u64) -> Result<()> {
    let iface = cfg.probe.iface.clone();
    let src_mac = match cfg.src_mac_override()? {
        Some(m) => m,
        None => pipeline::iface_mac(&iface)?,
    };
    let dst_mac = cfg.esp_mac_bytes()?;
    let esp = cfg.target.esp_ip;
    let sport = cfg.target.stim_sport;
    tokio::task::spawn_blocking(move || {
        let raw = pipeline::RawTx::open(&iface, src_mac, dst_mac, esp, PORT, sport)?;
        raw.announce_neighbor(STIM_SRC)?;
        for _ in 0..n {
            raw.send_udp(STIM_SRC, esp, PORT, b"CASE")?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("stimulus thread")?
}
