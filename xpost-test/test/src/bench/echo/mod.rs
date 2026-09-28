//! Pure XDP_TX echo baseline (`test echo`).
//!
//! Sweeps probe intervals; records bounce RTT only (`xdp_echo`). Listing 1 Mpps
//! XDP full-swap bounce RTT (`test echo`). Drop throughput is `drop-xdp` / `drop-hal`.

mod pg;

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use log::{info, warn};
use rand::RngExt;
use test_common::hot_load_op::{
    self, DATAPATH_DISABLE, DATAPATH_ENABLE, HotLoadOp, KIND_DATAPATH, KIND_READBACK,
    STORAGE_SRAM_A, swap_rx_op,
};

use crate::harness::Harness;

use super::pg_dsn::PgOpts;
use super::reset::reboot_dut;
use super::stop::{self, Stop};
use super::wl_common::fmt_dur;

mod assets {
    include!(concat!(env!("OUT_DIR"), "/xdp_echo_assets.rs"));
}

const SEND_WINDOW: Duration = Duration::from_secs(10);
const SMOKE_WINDOW: Duration = Duration::from_secs(3);
const READY_POLL: Duration = Duration::from_secs(90);
const POST_READY: Duration = Duration::from_secs(30);
const U_ECHO_WAIT: Duration = Duration::from_millis(100);
const U_ECHO_STREAK: u32 = 10;
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const UDP_PAYLOAD_LEN: i32 = 34;
const INTERVAL_LO_US: u32 = 0;
const INTERVAL_MID_US: u32 = 2_000;
const INTERVAL_HI_US: u32 = 4_000;
const N_ECHO_TASKS: usize = 1_000;
const N_DENSE: usize = 600;
const N_SPARSE: usize = 400;

#[derive(Clone, Copy, Debug)]
struct Task {
    interval_us: u32,
}

fn echo_intervals(smoke: bool) -> Vec<u32> {
    if smoke {
        return vec![INTERVAL_LO_US, INTERVAL_MID_US, INTERVAL_HI_US];
    }
    debug_assert_eq!(N_DENSE + N_SPARSE, N_ECHO_TASKS);
    let mut v = Vec::with_capacity(N_ECHO_TASKS);
    for i in 0..N_DENSE {
        let t = if N_DENSE == 1 {
            0.0
        } else {
            i as f64 / (N_DENSE - 1) as f64
        };
        let us = INTERVAL_LO_US as f64 + (INTERVAL_MID_US - INTERVAL_LO_US) as f64 * t;
        v.push(us.round() as u32);
    }
    for i in 1..=N_SPARSE {
        let t = i as f64 / N_SPARSE as f64;
        let us = INTERVAL_MID_US as f64 + (INTERVAL_HI_US - INTERVAL_MID_US) as f64 * t;
        v.push(us.round() as u32);
    }
    v.dedup();
    v
}

pub async fn run_echo(harness: &mut Harness, smoke: bool, formal: bool) -> Result<()> {
    let stop = Stop::new();
    stop::spawn_watch(stop.clone());

    let Some(mut pg) = pg::Pg::connect_retry(PgOpts { formal }, &stop).await else {
        eprintln!("stopped before postgres");
        return Ok(());
    };

    let intervals = echo_intervals(smoke);
    let window = if smoke { SMOKE_WINDOW } else { SEND_WINDOW };
    let tasks: Vec<Task> = intervals
        .iter()
        .copied()
        .map(|interval_us| Task { interval_us })
        .collect();

    let n_le_mid = tasks
        .iter()
        .filter(|t| t.interval_us <= INTERVAL_MID_US)
        .count();
    let n_gt_mid = tasks.len() - n_le_mid;
    eprintln!(
        "echo {} rounds  interval_us {}..={} (mid={}, dense≤mid≈{n_le_mid} sparse>{n_gt_mid})  window={}s  → xdp_echo_*  formal={formal}",
        tasks.len(),
        INTERVAL_LO_US,
        INTERVAL_HI_US,
        INTERVAL_MID_US,
        window.as_secs()
    );

    let ctrl = gen_lan_ip(harness.esp_ip, &[harness.esp_ip]);
    info!("echo ctrl={ctrl} esp={} smoke={smoke}", harness.esp_ip);

    let mut need_reboot = false;
    let mut done = 0usize;
    let started = Instant::now();
    let total = tasks.len();

    for task in &tasks {
        if stop.is_set() {
            break;
        }
        eprintln!(
            "No. {}/{total}  interval={}us …",
            done + 1,
            task.interval_us
        );
        let (ok_n, probes, config_id) = exec_round(
            harness,
            ctrl,
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
            eprintln!("DUT did not come back; stopping echo");
            break;
        }
        eprintln!("  → bounce {ok_n}/{probes}  pg={config_id:?}");
        done += 1;
        need_reboot = true;
        if stop.is_set() {
            break;
        }
    }
    eprintln!(
        "echo done  {done}/{total}  elapsed {}",
        fmt_dur(started.elapsed())
    );
    Ok(())
}

async fn exec_round(
    harness: &mut Harness,
    ctrl: Ipv4Addr,
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
            if !reboot_until_ready(harness, ctrl, stop).await {
                return Ok((0, 0, None));
            }
            if !wait_u_echo(harness, ctrl, post_ready, stop).await {
                if stop.is_set() {
                    return Ok((0, 0, None));
                }
                tries += 1;
                if tries >= TRIES {
                    bail!("U echo not stable after {TRIES} reboots");
                }
                warn!("U echo not stable; reboot ({tries}/{TRIES})");
                need_reboot = true;
                continue;
            }
        }
        match reconcile(harness, ctrl).await {
            Ok(()) => break,
            Err(e) => {
                tries += 1;
                if tries >= TRIES {
                    return Err(e).context(format!("echo reconcile failed after {TRIES} tries"));
                }
                warn!("echo reconcile failed ({e:#}); reboot ({tries}/{TRIES})");
                need_reboot = true;
            }
        }
    }

    let samples = probe_bounce(harness, ctrl, task.interval_us, window, stop).await;
    let probes = samples.len() as u32;
    let ok_n = samples.iter().filter(|s| s.ok).count() as u32;

    let mut config_id = None;
    if let Some(pg) = pg.as_mut() {
        config_id = pg
            .ingest_until_ok(
                task.interval_us as i32,
                UDP_PAYLOAD_LEN,
                window.as_secs() as i32,
                PROBE_TIMEOUT.as_millis() as i32,
                &samples,
                stop,
            )
            .await;
    }
    harness.reset_state().await;
    Ok((ok_n, probes, config_id))
}

async fn probe_bounce(
    harness: &mut Harness,
    src: Ipv4Addr,
    interval_us: u32,
    window: Duration,
    stop: &Stop,
) -> Vec<pg::ProbeRtt> {
    let start = Instant::now();
    let deadline = start + window;
    let mut samples = Vec::new();
    let mut next = start;
    let mut seq_i = 0i32;

    while Instant::now() < deadline {
        if stop.is_set() {
            break;
        }
        if interval_us > 0 {
            let now = Instant::now();
            if now < next {
                stop.sleep_or(next.saturating_duration_since(now)).await;
            }
        }
        if stop.is_set() || Instant::now() >= deadline {
            break;
        }

        let seq = harness.alloc_seq();
        let t_wall = SystemTime::now();
        let ok = match harness.send_stimulus(src, None, seq).await {
            Ok(()) => {
                let t0 = harness
                    .wait_t0(seq, Some(PROBE_TIMEOUT))
                    .await
                    .ok()
                    .flatten();
                let bounce = harness
                    .wait_bounce(seq, Some(PROBE_TIMEOUT))
                    .await
                    .ok()
                    .flatten();
                match (t0, bounce) {
                    (Some(t0), Some(t1)) => {
                        let rtt_ms = (t1.saturating_sub(t0)) as f64 / 1_000_000.0;
                        samples.push(pg::ProbeRtt {
                            t0: t_wall,
                            seq: seq_i,
                            ok: true,
                            rtt_ms: Some(rtt_ms),
                        });
                        seq_i += 1;
                        let _ = harness.take_seq(seq).await;
                        if interval_us > 0 {
                            next += Duration::from_micros(u64::from(interval_us));
                        }
                        continue;
                    }
                    _ => false,
                }
            }
            Err(_) => false,
        };
        let _ = harness.take_seq(seq).await;
        samples.push(pg::ProbeRtt {
            t0: t_wall,
            seq: seq_i,
            ok,
            rtt_ms: None,
        });
        seq_i += 1;
        if interval_us > 0 {
            next += Duration::from_micros(u64::from(interval_us));
        } else {
            tokio::task::yield_now().await;
        }
    }
    samples
}

async fn reconcile(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    datapath(harness, ctrl, DATAPATH_DISABLE)
        .await
        .context("disable")?;
    let rb = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb as usize != assets::BIN.len() {
        swap_aot(harness, ctrl).await.context("SWAP")?;
        datapath(harness, ctrl, DATAPATH_DISABLE).await.ok();
    }
    let rb2 = readback_len(harness, ctrl).await.unwrap_or(0);
    if rb2 as usize != assets::BIN.len() {
        bail!("confirm READBACK len={rb2} want {}", assets::BIN.len());
    }
    info!("echo reconcile ok xdp_len={rb2}");
    datapath(harness, ctrl, DATAPATH_ENABLE)
        .await
        .context("enable")?;
    Ok(())
}

async fn swap_aot(harness: &Harness, ctrl: Ipv4Addr) -> Result<()> {
    let ack = harness
        .hot_load_op(ctrl, swap_rx_op(0, assets::BIN, 0))
        .await?;
    if ack.status != hot_load_op::STATUS_OK {
        bail!("SWAP status={}", ack.status);
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
    reboot_dut(harness, ctrl, "echo", stop).await;
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
    for try_n in 1..=3u32 {
        if stop.is_set() {
            return false;
        }
        reboot(harness, ctrl, stop).await;
        if wait_ready(harness, ctrl, stop).await {
            return true;
        }
        warn!("echo DUT not ready in {:?} (try {try_n}/3)", READY_POLL);
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

fn gen_lan_ip(esp: Ipv4Addr, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let mut rng = rand::rng();
    let oct = esp.octets();
    loop {
        let host = rng.random_range(2..254u8);
        let ip = Ipv4Addr::new(oct[0], oct[1], oct[2], host);
        if ip != esp && !avoid.contains(&ip) {
            return ip;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_intervals_full_grid() {
        let v = echo_intervals(false);
        assert_eq!(v.first(), Some(&0));
        assert_eq!(v.last(), Some(&INTERVAL_HI_US));
        assert!(v.windows(2).all(|w| w[0] <= w[1]));
        assert!((990..=N_ECHO_TASKS).contains(&v.len()), "len={}", v.len());
    }

    #[test]
    fn echo_intervals_smoke() {
        assert_eq!(
            echo_intervals(true),
            vec![INTERVAL_LO_US, INTERVAL_MID_US, INTERVAL_HI_US]
        );
    }
}
