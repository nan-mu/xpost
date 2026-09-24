//! Whitelist under-attack RTT (`test wl-rtt`).
//!
//! Fixed legit interval 2.2 ms; sweeps α including 0. Userspace echo via ddos_wl PASS.
//! Postgres: `xdp_ddos_config` / `xdp_ddos_data`.

mod pg;

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use log::{info, warn};
use test_common::hot_load_op::{
    self, DATAPATH_DISABLE, DATAPATH_ENABLE, HotLoadOp, KIND_DATAPATH, KIND_READBACK,
    STORAGE_SRAM_A, swap_rx_op,
};
use test_common::map_crud_op::{self, MapCrudOp};

use crate::harness::Harness;

use super::hping::Hping;
use super::pg_dsn::PgOpts;
use super::reset::reboot_dut;
use super::stop::{self, Stop};
use super::wl_common::{
    AlphaTask, INTERFERER, LEGIT_INTERVAL_US, alpha_tasks, fmt_dur, hping_pps, ip_key, pick_peers,
};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/ddos_wl_assets.rs"));
}

const MAP_NAME: &str = "ddos_wl";
const SEND_WINDOW: Duration = Duration::from_secs(10);
const SMOKE_WINDOW: Duration = Duration::from_secs(3);
const READY_POLL: Duration = Duration::from_secs(90);
const POST_READY: Duration = Duration::from_secs(30);
const U_ECHO_WAIT: Duration = Duration::from_millis(100);
const U_ECHO_STREAK: u32 = 10;
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const MAP_KEY_SIZE: u32 = 4;
const MAP_VAL_SIZE: u32 = 1;
const MAP_MAX_ENTRIES: u32 = 64;
const UDP_PAYLOAD_LEN: i32 = 34;

#[derive(Clone, Copy, Debug)]
struct Task {
    alpha_x100: u32,
}

pub async fn run_wl_rtt(harness: &mut Harness, smoke: bool, formal: bool) -> Result<()> {
    let stop = Stop::new();
    stop::spawn_watch(stop.clone());

    let Some(mut pg) = pg::Pg::connect_retry(PgOpts { formal }, &stop).await else {
        eprintln!("stopped before postgres");
        return Ok(());
    };

    let tasks: Vec<Task> = alpha_tasks(smoke)
        .into_iter()
        .map(|AlphaTask { alpha_x100 }| Task { alpha_x100 })
        .collect();
    let window = if smoke { SMOKE_WINDOW } else { SEND_WINDOW };
    let total = tasks.len();

    eprintln!(
        "wl-rtt {total} rounds  interval={LEGIT_INTERVAL_US}us × α×100 {:?}  → xdp_ddos_*  formal={formal}",
        tasks.iter().map(|t| t.alpha_x100).collect::<Vec<_>>()
    );

    let peers = pick_peers(harness.esp_ip);
    info!(
        "wl-rtt peers ctrl={} wl={:?} atk={:?} esp={} smoke={smoke}",
        peers.ctrl, peers.wl, peers.atk, harness.esp_ip
    );

    let mut need_reboot = false;
    let mut done = 0usize;
    let started = Instant::now();

    for task in &tasks {
        if stop.is_set() {
            break;
        }
        let pps = hping_pps(task.alpha_x100);
        eprintln!(
            "No. {}/{total}  α×100={}  hping≈{pps}pps …",
            done + 1,
            task.alpha_x100
        );
        let (ok_n, probes, config_id) = exec_round(
            harness,
            &peers,
            *task,
            window,
            POST_READY,
            &stop,
            need_reboot,
            Some(&mut pg),
        )
        .await?;
        if stop.is_set() && probes == 0 {
            break;
        }
        if need_reboot && probes == 0 {
            eprintln!("DUT did not come back; stopping wl-rtt");
            break;
        }
        eprintln!("  → U {ok_n}/{probes}  pg={config_id:?}");
        done += 1;
        need_reboot = true;
        if stop.is_set() {
            break;
        }
    }
    eprintln!(
        "wl-rtt done  {done}/{total}  elapsed {}",
        fmt_dur(started.elapsed())
    );
    Ok(())
}

async fn exec_round(
    harness: &mut Harness,
    peers: &super::wl_common::Peers,
    task: Task,
    window: Duration,
    post_ready: Duration,
    stop: &Stop,
    mut need_reboot: bool,
    mut pg: Option<&mut pg::Pg>,
) -> Result<(u32, u32, Option<i64>)> {
    const TRIES: u32 = 3;
    let mut tries = 0u32;
    loop {
        if stop.is_set() {
            return Ok((0, 0, None));
        }
        if need_reboot {
            if !reboot_until_ready(harness, peers.ctrl, stop).await {
                return Ok((0, 0, None));
            }
            if !wait_u_echo(harness, peers.ctrl, post_ready, stop).await {
                if stop.is_set() {
                    return Ok((0, 0, None));
                }
                tries += 1;
                if tries >= TRIES {
                    bail!("U echo not stable after {TRIES} reboots");
                }
                need_reboot = true;
                continue;
            }
        }
        match reconcile(harness, peers).await {
            Ok(()) => break,
            Err(e) => {
                tries += 1;
                if tries >= TRIES {
                    return Err(e).context(format!("wl-rtt reconcile failed after {TRIES}"));
                }
                warn!("wl-rtt reconcile failed ({e:#}); reboot");
                need_reboot = true;
            }
        }
    }

    let pps = hping_pps(task.alpha_x100);
    let atk = peers.atk[0];
    let hping = match Hping::start(harness.esp_ip, atk, harness.echo_port, pps) {
        Ok(h) => h,
        Err(e) => {
            warn!("hping3 start failed ({e:#}); continuing without flood");
            Hping::start(harness.esp_ip, atk, harness.echo_port, 0)?
        }
    };

    let src = peers.wl[0];
    let samples = probe_u(harness, src, window, stop).await;
    hping.stop();

    let probes = samples.len() as u32;
    let ok_n = samples.iter().filter(|s| s.reply == "U").count() as u32;

    let mut config_id = None;
    if let Some(pg) = pg.as_mut() {
        config_id = pg
            .ingest_until_ok(
                LEGIT_INTERVAL_US as i32,
                UDP_PAYLOAD_LEN,
                "ddos",
                task.alpha_x100 as i32,
                &samples,
                stop,
            )
            .await;
    }
    let _ = INTERFERER;
    harness.reset_state().await;
    Ok((ok_n, probes, config_id))
}

async fn probe_u(
    harness: &mut Harness,
    src: Ipv4Addr,
    window: Duration,
    stop: &Stop,
) -> Vec<pg::ProbeRow> {
    let start = Instant::now();
    let deadline = start + window;
    let gap = Duration::from_micros(u64::from(LEGIT_INTERVAL_US));
    let mut samples = Vec::new();
    let mut next = start;

    while Instant::now() < deadline {
        if stop.is_set() {
            break;
        }
        let now = Instant::now();
        if now < next {
            stop.sleep_or(next.saturating_duration_since(now)).await;
        }
        if stop.is_set() || Instant::now() >= deadline {
            break;
        }

        let seq = harness.alloc_seq();
        let t_wall = SystemTime::now();
        let t_send = Instant::now();
        let ok = match harness.send_stimulus(src, None, seq).await {
            Ok(()) => harness
                .wait_echo(seq, Some(PROBE_TIMEOUT))
                .await
                .ok()
                .flatten()
                .is_some(),
            Err(_) => false,
        };
        let _ = harness.take_seq(seq).await;
        let (reply, rtt_ms) = if ok {
            ("U", Some(t_send.elapsed().as_secs_f64() * 1000.0))
        } else {
            ("NU", None)
        };
        samples.push(pg::ProbeRow {
            t0: t_wall,
            reply,
            rtt_ms,
        });
        next += gap;
    }
    samples
}

async fn reconcile(harness: &Harness, peers: &super::wl_common::Peers) -> Result<()> {
    let ctrl = peers.ctrl;
    datapath(harness, ctrl, DATAPATH_DISABLE)
        .await
        .context("disable")?;

    let rb = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb as usize != assets::BIN.len() {
        swap_aot(harness, ctrl).await.context("SWAP")?;
        datapath(harness, ctrl, DATAPATH_DISABLE).await.ok();
    }

    let probe = lookup(harness, ctrl, &ip_key(ctrl)).await;
    let map_missing = match &probe {
        Ok(ack) => ack.status == map_crud_op::STATUS_NOT_FOUND,
        Err(e) => bail!("LOOKUP {MAP_NAME}: {e:#}"),
    };
    if map_missing {
        create_map(harness, ctrl).await.context("CREATE")?;
        datapath(harness, ctrl, DATAPATH_DISABLE)
            .await
            .context("disable after CREATE")?;
    }

    update(harness, ctrl, ctrl).await.context("UPDATE ctrl")?;
    for ip in &peers.wl {
        update(harness, ctrl, *ip)
            .await
            .with_context(|| format!("UPDATE wl {ip}"))?;
    }

    let ctrl_ack = lookup(harness, ctrl, &ip_key(ctrl)).await?;
    if !lookup_ok(&ctrl_ack) {
        bail!("confirm ctrl {ctrl} missing");
    }
    for ip in &peers.wl {
        let ack = lookup(harness, ctrl, &ip_key(*ip)).await?;
        if !lookup_ok(&ack) {
            bail!("confirm wl {ip} missing");
        }
    }

    let rb2 = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb2 as usize != assets::BIN.len() {
        bail!("confirm READBACK len={rb2} want {}", assets::BIN.len());
    }
    info!(
        "wl-rtt reconcile ok xdp_len={rb2} ctrl={} wl_n={}",
        peers.ctrl,
        peers.wl.len()
    );
    datapath(harness, ctrl, DATAPATH_ENABLE)
        .await
        .context("enable")?;
    Ok(())
}

async fn lookup(
    harness: &Harness,
    ctrl: Ipv4Addr,
    key: &[u8],
) -> Result<test_common::map_crud_op::MapCrudAck<'static>> {
    harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_LOOKUP,
                MAP_NAME,
                map_crud_op::MAP_TYPE_HASH,
                key,
                &[0u8],
                0,
            ),
        )
        .await
}

async fn update(harness: &Harness, ctrl: Ipv4Addr, ip: Ipv4Addr) -> Result<()> {
    let key = ip_key(ip);
    let value = [1u8];
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_UPDATE,
                MAP_NAME,
                map_crud_op::MAP_TYPE_HASH,
                &key,
                &value,
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("UPDATE {ip} status={}", ack.status);
    }
    Ok(())
}

async fn create_map(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp {
                kind: map_crud_op::KIND_CREATE,
                map_name: MAP_NAME,
                map_type: map_crud_op::MAP_TYPE_HASH,
                key: &[],
                value: &[],
                flags: 0,
                key_size: MAP_KEY_SIZE,
                value_size: MAP_VAL_SIZE,
                max_entries: MAP_MAX_ENTRIES,
                aot_addr: assets::AOT_ADDR,
            },
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK && ack.status != map_crud_op::STATUS_CONFLICT {
        bail!("CREATE {MAP_NAME} status={} {}", ack.status, ack.reason);
    }
    Ok(())
}

async fn swap_aot(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    let ack = harness
        .hot_load_op(ctrl, swap_rx_op(0, assets::BIN, 0))
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("SWAP status={} {}", ack.status, ack.reason);
    }
    Ok(())
}

fn lookup_ok(ack: &test_common::map_crud_op::MapCrudAck<'_>) -> bool {
    ack.status == map_crud_op::STATUS_OK && !ack.value.is_empty()
}

async fn datapath(harness: &Harness, ctrl: Ipv4Addr, region: u32) -> Result<()> {
    let ack = harness
        .hot_load_op(
            ctrl,
            HotLoadOp {
                kind: KIND_DATAPATH,
                region,
                dram_slot: 0,
                src: 0,
                dst: 0,
                payload: &[],
            },
        )
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("DATAPATH region={region} status={}", ack.status);
    }
    Ok(())
}

async fn readback_len_wait(harness: &Harness, ctrl: Ipv4Addr, wait: Duration) -> Result<u32> {
    let ack = harness
        .hot_load_op_wait(
            ctrl,
            HotLoadOp {
                kind: KIND_READBACK,
                region: 0,
                dram_slot: 0,
                src: STORAGE_SRAM_A,
                dst: 0,
                payload: &[],
            },
            wait,
        )
        .await?;
    Ok(ack.readback_len)
}

async fn readback_len(harness: &Harness, ctrl: Ipv4Addr) -> Result<u32> {
    readback_len_wait(harness, ctrl, harness.pair_timeout()).await
}

async fn reboot(harness: &Harness, ctrl: Ipv4Addr, stop: &Stop) {
    reboot_dut(harness, ctrl, "wl-rtt", stop).await;
}

async fn wait_ready(harness: &Harness, ctrl: Ipv4Addr, stop: &Stop) -> bool {
    const PROBE: Duration = Duration::from_millis(80);
    let start = Instant::now();
    while start.elapsed() < READY_POLL {
        if stop.is_set() {
            return false;
        }
        if readback_len_wait(harness, ctrl, PROBE).await.is_ok() {
            return true;
        }
    }
    false
}

async fn reboot_until_ready(harness: &Harness, ctrl: Ipv4Addr, stop: &Stop) -> bool {
    for _ in 1..=3u32 {
        if stop.is_set() {
            return false;
        }
        reboot(harness, ctrl, stop).await;
        if wait_ready(harness, ctrl, stop).await {
            return true;
        }
    }
    false
}

async fn wait_u_echo(harness: &mut Harness, src: Ipv4Addr, budget: Duration, stop: &Stop) -> bool {
    let t0 = Instant::now();
    let mut streak = 0u32;
    while t0.elapsed() < budget {
        if stop.is_set() {
            return false;
        }
        let seq = harness.alloc_seq();
        let ok = match harness.send_stimulus(src, None, seq).await {
            Ok(()) => harness
                .wait_echo(seq, Some(U_ECHO_WAIT))
                .await
                .ok()
                .flatten()
                .is_some(),
            Err(_) => false,
        };
        let _ = harness.take_seq(seq).await;
        if ok {
            streak += 1;
            if streak >= U_ECHO_STREAK {
                harness.reset_state().await;
                return true;
            }
        } else {
            streak = 0;
        }
    }
    false
}
