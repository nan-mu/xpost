//! Shared setup and task grid for rate-blacklist benchmarks.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{info, warn};
use rand::RngExt;
use test_common::hot_load_op::{
    self, DATAPATH_DISABLE, DATAPATH_ENABLE, HotLoadOp, KIND_DATAPATH, KIND_READBACK,
    STORAGE_SRAM_A, swap_rx_op,
};
use test_common::map_crud_op::{self, MapCrudOp};

use crate::harness::Harness;

use super::hping::Hping;
use super::reset::reboot_dut;
use super::stop::Stop;

mod assets {
    include!(concat!(env!("OUT_DIR"), "/ddos_rate_bl_assets.rs"));
}

const MAP_CTRL_WL: &str = "ctrl_wl";
const MAP_RATE_STATE: &str = "rate_state";
const MAP_PARAMS: &str = "params";
const MAP_STATS: &str = "stats";
const RATE_ENT_SIZE: u32 = 24;

pub const P_PEAK: u32 = 2_000;
pub const THRESH: u32 = 200;
pub const PEAK_PCTS: [u32; 5] = [0, 25, 50, 75, 100];
pub const CELL_REPEATS: u32 = 3;
pub const INTERFERER: &str = "hping3";
pub const SEND_WINDOW: Duration = Duration::from_secs(10);
pub const SMOKE_RTT_WINDOW: Duration = Duration::from_secs(5);
pub const SMOKE_LOSS_WINDOW: Duration = Duration::from_secs(3);
pub const POST_READY: Duration = Duration::from_secs(30);

const READY_POLL: Duration = Duration::from_secs(90);
const U_ECHO_WAIT: Duration = Duration::from_millis(100);
const U_ECHO_STREAK: u32 = 10;

#[derive(Clone, Copy, Debug)]
pub struct Task {
    pub peak_pct: u32,
    pub mitigation_on: bool,
}

#[derive(Clone)]
pub struct Peers {
    pub ctrl: Ipv4Addr,
    pub atk: Vec<Ipv4Addr>,
}

pub fn attack_pps(peak_pct: u32) -> u32 {
    ((u64::from(P_PEAK) * u64::from(peak_pct)) / 100) as u32
}

pub fn tasks(smoke: bool) -> Vec<Task> {
    let pcts: &[u32] = if smoke { &[50] } else { &PEAK_PCTS };
    let mut out = Vec::with_capacity(pcts.len() * 2 * CELL_REPEATS as usize);
    for _ in 0..CELL_REPEATS {
        for &peak_pct in pcts {
            for mitigation_on in [false, true] {
                out.push(Task {
                    peak_pct,
                    mitigation_on,
                });
            }
        }
    }
    out
}

pub fn pick_peers(esp: Ipv4Addr) -> Peers {
    let mut avoid = vec![esp];
    let ctrl = gen_lan_ip(esp, &avoid);
    avoid.push(ctrl);
    let mut atk = Vec::with_capacity(4);
    for _ in 0..4 {
        let ip = gen_lan_ip(esp, &avoid);
        avoid.push(ip);
        atk.push(ip);
    }
    Peers { ctrl, atk }
}

fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let mut rng = rand::rng();
    let oct = esp.octets();
    loop {
        let ip = Ipv4Addr::new(oct[0], oct[1], oct[2], rng.random_range(2..254u8));
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
}

fn ip_key(ip: Ipv4Addr) -> [u8; 4] {
    ip.octets()
}

pub fn start_hping(harness: &Harness, peers: &Peers, pps: u32) -> Result<Hping> {
    let atk = peers.atk[0];
    match Hping::start(harness.esp_ip, atk, harness.echo_port, pps) {
        Ok(h) => Ok(h),
        Err(e) => {
            warn!("hping3 start failed ({e:#}); continuing without interferer");
            Hping::start(harness.esp_ip, atk, harness.echo_port, 0)
        }
    }
}

pub async fn prepare_round(
    harness: &mut Harness,
    peers: &Peers,
    stop: &Stop,
    mut need_reboot: bool,
) -> Result<bool> {
    const TRIES: u32 = 3;
    let mut tries = 0;
    loop {
        if stop.is_set() {
            return Ok(false);
        }
        if need_reboot {
            if !reboot_until_ready(harness, peers.ctrl, stop).await {
                return Ok(false);
            }
            if !wait_u_echo(harness, peers.ctrl, POST_READY, stop).await {
                if stop.is_set() {
                    return Ok(false);
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
            Ok(()) => return Ok(true),
            Err(e) => {
                tries += 1;
                if tries >= TRIES {
                    return Err(e).context(format!(
                        "rate blacklist reconcile failed after {TRIES} tries"
                    ));
                }
                warn!("rate blacklist reconcile failed ({e:#}); reboot ({tries}/{TRIES})");
                need_reboot = true;
            }
        }
    }
}

pub async fn set_mitigation(harness: &Harness, ctrl: Ipv4Addr, enabled: bool) -> Result<()> {
    if !enabled {
        datapath(harness, ctrl, DATAPATH_DISABLE)
            .await
            .context("mitigation off")?;
    }
    Ok(())
}

async fn reconcile(harness: &Harness, peers: &Peers) -> Result<()> {
    let ctrl = peers.ctrl;
    datapath(harness, ctrl, DATAPATH_DISABLE).await?;
    let rb = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb as usize != assets::BIN.len() {
        swap_aot(harness, ctrl).await?;
        datapath(harness, ctrl, DATAPATH_DISABLE).await.ok();
    }
    create_hash(harness, ctrl, MAP_CTRL_WL, 16, 1, assets::CTRL_WL_AOT).await?;
    create_hash(
        harness,
        ctrl,
        MAP_RATE_STATE,
        64,
        RATE_ENT_SIZE,
        assets::RATE_STATE_AOT,
    )
    .await?;
    create_array(harness, ctrl, MAP_PARAMS, 4, assets::PARAMS_AOT).await?;
    create_array(harness, ctrl, MAP_STATS, 4, assets::STATS_AOT).await?;
    datapath(harness, ctrl, DATAPATH_DISABLE).await?;
    update_ctrl_wl(harness, ctrl, ctrl).await?;
    update_array(harness, ctrl, MAP_PARAMS, 0, THRESH).await?;
    for idx in 0..4 {
        update_array(harness, ctrl, MAP_STATS, idx, 0).await?;
    }

    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_LOOKUP,
                MAP_CTRL_WL,
                map_crud_op::MAP_TYPE_HASH,
                &ip_key(ctrl),
                &[0],
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK || ack.value.is_empty() {
        bail!("confirm ctrl_wl missing {ctrl}");
    }
    for atk in &peers.atk {
        let ack = harness
            .map_crud_op(
                ctrl,
                MapCrudOp::simple(
                    map_crud_op::KIND_LOOKUP,
                    MAP_CTRL_WL,
                    map_crud_op::MAP_TYPE_HASH,
                    &ip_key(*atk),
                    &[0],
                    0,
                ),
            )
            .await?;
        if ack.status == map_crud_op::STATUS_OK && !ack.value.is_empty() {
            bail!("attack ip {atk} unexpectedly in ctrl_wl");
        }
    }
    let rb = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb as usize != assets::BIN.len() {
        bail!("confirm READBACK len={rb} want {}", assets::BIN.len());
    }
    info!("rate blacklist reconcile ok xdp_len={rb} ctrl_wl={ctrl} thresh={THRESH}");
    datapath(harness, ctrl, DATAPATH_ENABLE).await
}

async fn create_hash(
    harness: &Harness,
    ctrl: Ipv4Addr,
    name: &str,
    max_entries: u32,
    value_size: u32,
    aot_addr: u64,
) -> Result<()> {
    create_map(
        harness,
        ctrl,
        name,
        map_crud_op::MAP_TYPE_HASH,
        max_entries,
        value_size,
        aot_addr,
    )
    .await
}

async fn create_array(
    harness: &Harness,
    ctrl: Ipv4Addr,
    name: &str,
    max_entries: u32,
    aot_addr: u64,
) -> Result<()> {
    create_map(
        harness,
        ctrl,
        name,
        map_crud_op::MAP_TYPE_ARRAY,
        max_entries,
        4,
        aot_addr,
    )
    .await
}

async fn create_map(
    harness: &Harness,
    ctrl: Ipv4Addr,
    name: &str,
    map_type: u32,
    max_entries: u32,
    value_size: u32,
    aot_addr: u64,
) -> Result<()> {
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp {
                kind: map_crud_op::KIND_CREATE,
                map_name: name,
                map_type,
                key: &[],
                value: &[],
                flags: 0,
                key_size: 4,
                value_size,
                max_entries,
                aot_addr,
            },
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK && ack.status != map_crud_op::STATUS_CONFLICT {
        bail!("CREATE {name} status={} {}", ack.status, ack.reason);
    }
    Ok(())
}

async fn update_ctrl_wl(harness: &Harness, ctrl: Ipv4Addr, ip: Ipv4Addr) -> Result<()> {
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_UPDATE,
                MAP_CTRL_WL,
                map_crud_op::MAP_TYPE_HASH,
                &ip_key(ip),
                &[1],
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("UPDATE {MAP_CTRL_WL} {ip} status={}", ack.status);
    }
    Ok(())
}

async fn update_array(
    harness: &Harness,
    ctrl: Ipv4Addr,
    name: &str,
    idx: u32,
    value: u32,
) -> Result<()> {
    let ack = harness
        .map_crud_op(
            ctrl,
            MapCrudOp::simple(
                map_crud_op::KIND_UPDATE,
                name,
                map_crud_op::MAP_TYPE_ARRAY,
                &idx.to_ne_bytes(),
                &value.to_ne_bytes(),
                0,
            ),
        )
        .await?;
    if ack.status != map_crud_op::STATUS_OK {
        bail!("UPDATE {name}[{idx}] status={}", ack.status);
    }
    Ok(())
}

async fn swap_aot(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    const MAX_SWAP_AOT: usize = 1400;
    if assets::BIN.len() > MAX_SWAP_AOT {
        bail!(
            "AOT {}B exceeds SWAP limit {MAX_SWAP_AOT}B",
            assets::BIN.len()
        );
    }
    let ack = harness
        .hot_load_op(ctrl, swap_rx_op(0, assets::BIN, 0))
        .await?;
    if ack.status != hot_load_op::STATUS_OK || ack.loaded_len as usize != assets::BIN.len() {
        bail!(
            "SWAP status={} loaded_len={} want={}",
            ack.status,
            ack.loaded_len,
            assets::BIN.len()
        );
    }
    Ok(())
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
    reboot_dut(harness, ctrl, "rate blacklist", stop).await;
}

async fn reboot_until_ready(harness: &Harness, ctrl: Ipv4Addr, stop: &Stop) -> bool {
    for try_n in 1..=3 {
        if stop.is_set() {
            return false;
        }
        reboot(harness, ctrl, stop).await;
        let started = Instant::now();
        while started.elapsed() < READY_POLL {
            if stop.is_set() {
                return false;
            }
            if readback_len_wait(harness, ctrl, Duration::from_millis(80))
                .await
                .is_ok()
            {
                return true;
            }
        }
        warn!("rate blacklist DUT not ready (try {try_n}/3)");
    }
    false
}

async fn wait_u_echo(harness: &mut Harness, src: Ipv4Addr, budget: Duration, stop: &Stop) -> bool {
    let started = Instant::now();
    let mut streak = 0;
    while started.elapsed() < budget {
        if stop.is_set() {
            return false;
        }
        let seq = harness.alloc_seq();
        let ok = harness.send_stimulus(src, None, seq).await.is_ok()
            && harness
                .wait_echo(seq, Some(U_ECHO_WAIT))
                .await
                .ok()
                .flatten()
                .is_some();
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

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}h{:02}m{:02}s", s / 3600, (s % 3600) / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attack_rate_scales() {
        assert_eq!(attack_pps(0), 0);
        assert_eq!(attack_pps(50), 1_000);
        assert_eq!(attack_pps(100), 2_000);
    }

    #[test]
    fn task_grid_repeats_cells() {
        assert_eq!(
            tasks(false).len(),
            PEAK_PCTS.len() * 2 * CELL_REPEATS as usize
        );
        assert_eq!(tasks(true).len(), 2 * CELL_REPEATS as usize);
    }
}
