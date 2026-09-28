//! Online FWSTIM U-echo RTT under peak-percentage rate-blacklist attack.

mod pg;

use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use log::info;

use crate::harness::Harness;

use super::pg_dsn::PgOpts;
use super::rate_common::{
    self, CELL_REPEATS, P_PEAK, PEAK_PCTS, POST_READY, SEND_WINDOW, SMOKE_RTT_WINDOW, THRESH,
};
use super::stop::{self, Stop};

const PROBE_INTERVAL: Duration = Duration::from_millis(50);
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

pub async fn run_rate_rtt(harness: &mut Harness, smoke: bool, formal: bool) -> Result<()> {
    let stop = Stop::new();
    stop::spawn_watch(stop.clone());
    let Some(mut pg) = pg::Pg::connect_retry(PgOpts { formal }, &stop).await else {
        return Ok(());
    };
    let plan = rate_common::tasks(smoke);
    let total = plan.len();
    let window = if smoke { SMOKE_RTT_WINDOW } else { SEND_WINDOW };
    let peers = rate_common::pick_peers(harness.esp_ip);
    eprintln!(
        "rate-rtt {} rounds  P_peak={P_PEAK} thresh={THRESH} window={}s peak%={PEAK_PCTS:?} × mit ×{CELL_REPEATS} → xdp_rate_rtt_*",
        total,
        window.as_secs()
    );
    info!(
        "rate-rtt peers ctrl={} atk={:?} esp={} post_ready={POST_READY:?}",
        peers.ctrl, peers.atk, harness.esp_ip
    );

    let started = Instant::now();
    let mut done = 0;
    let mut need_reboot = false;
    for task in plan {
        if stop.is_set() {
            break;
        }
        let pps = rate_common::attack_pps(task.peak_pct);
        eprintln!(
            "No. {}/{} peak%={} mit={} attack={}pps",
            done + 1,
            total,
            task.peak_pct,
            task.mitigation_on,
            pps
        );
        if !rate_common::prepare_round(harness, &peers, &stop, need_reboot).await? {
            break;
        }
        rate_common::set_mitigation(harness, peers.ctrl, task.mitigation_on).await?;
        let hping = rate_common::start_hping(harness, &peers, pps)?;
        let samples = probe(harness, peers.ctrl, window, &stop).await;
        hping.stop();
        if stop.is_set() && samples.is_empty() {
            break;
        }
        let ok = samples.iter().filter(|sample| sample.ok).count();
        let config_id = pg
            .ingest_until_ok(pps as i32, task, window.as_secs() as i32, &samples, &stop)
            .await;
        eprintln!("  → echo={ok}/{} pg={config_id:?}", samples.len());
        harness.reset_state().await;
        done += 1;
        need_reboot = true;
    }
    eprintln!(
        "rate-rtt done {done} elapsed {}",
        rate_common::fmt_dur(started.elapsed())
    );
    Ok(())
}

async fn probe(
    harness: &mut Harness,
    src: std::net::Ipv4Addr,
    window: Duration,
    stop: &Stop,
) -> Vec<pg::ProbeRtt> {
    let started = Instant::now();
    let deadline = started + window;
    let mut next = started;
    let mut samples = Vec::new();
    while Instant::now() < deadline && !stop.is_set() {
        let now = Instant::now();
        if now < next {
            stop.sleep_or(next - now).await;
        }
        if stop.is_set() || Instant::now() >= deadline {
            break;
        }
        let seq = harness.alloc_seq();
        let t0 = SystemTime::now();
        let sent = Instant::now();
        let ok = harness.send_stimulus(src, None, seq).await.is_ok()
            && harness
                .wait_echo(seq, Some(PROBE_TIMEOUT))
                .await
                .ok()
                .flatten()
                .is_some();
        let _ = harness.take_seq(seq).await;
        samples.push(pg::ProbeRtt {
            t0,
            seq: samples.len() as i32,
            ok,
            rtt_ms: ok.then(|| sent.elapsed().as_secs_f64() * 1_000.0),
        });
        next += PROBE_INTERVAL;
    }
    samples
}
