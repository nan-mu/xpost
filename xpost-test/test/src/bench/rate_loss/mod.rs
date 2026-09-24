//! Collect-then-dump byte loss under peak-percentage rate-blacklist attack.

mod pg;

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use log::{info, warn};
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use test_common::{COLLECT_CAP, COLLECT_HDR_LEN, MAGIC_COLLECT};

use crate::harness::Harness;

use super::pg_dsn::PgOpts;
use super::rate_common::{
    self, CELL_REPEATS, P_PEAK, PEAK_PCTS, SEND_WINDOW, SMOKE_LOSS_WINDOW, THRESH,
};
use super::stop::{self, Stop};
use super::wl_common::LEGIT_INTERVAL_US;

const UDP_PAYLOAD_LEN: usize = 64;
const STREAM_LEN: usize = COLLECT_CAP;
const DUMP_SETTLE: Duration = Duration::from_millis(500);
const DUMP_RETRIES: u32 = 5;
const DUMP_RETRY_GAP: Duration = Duration::from_millis(150);

#[derive(Default)]
struct RoundStats {
    sent_pkts: usize,
    recv_bytes: usize,
    match_bytes: usize,
    loss_pct: Option<f64>,
    dump_ok: bool,
    dump_attempts: u32,
    config_id: Option<i64>,
}

pub async fn run_rate_loss(harness: &mut Harness, smoke: bool, formal: bool) -> Result<()> {
    let stop = Stop::new();
    stop::spawn_watch(stop.clone());
    let Some(mut pg) = pg::Pg::connect_retry(PgOpts { formal }, &stop).await else {
        return Ok(());
    };
    let plan = rate_common::tasks(smoke);
    let total = plan.len();
    let window = if smoke {
        SMOKE_LOSS_WINDOW
    } else {
        SEND_WINDOW
    };
    let peers = rate_common::pick_peers(harness.esp_ip);
    eprintln!(
        "rate-loss {total} rounds P_peak={P_PEAK} thresh={THRESH} peak%={PEAK_PCTS:?} × mit ×{CELL_REPEATS} interval={LEGIT_INTERVAL_US}us → xdp_rate_loss_*"
    );
    info!(
        "rate-loss peers ctrl={} atk={:?}; collect source is ctrl_wl",
        peers.ctrl, peers.atk
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
        let seed = rand::rng().random::<u64>();
        let stream = gen_stream(seed, STREAM_LEN);
        if let Err(e) = harness.collect_reset(peers.ctrl).await {
            warn!("collect reset: {e:#}; continuing");
        }
        let hping = rate_common::start_hping(harness, &peers, pps)?;
        let sent = send_stream(harness, peers.ctrl, &stream, window, &stop)
            .await
            .unwrap_or_else(|e| {
                warn!("send collect stream: {e:#}");
                0
            });
        hping.stop();
        if stop.is_set() && sent == 0 {
            break;
        }

        stop.sleep_or(DUMP_SETTLE).await;
        let (recv, dump_ok, dump_attempts) = dump_stream(harness, peers.ctrl, &stop).await;
        let match_bytes = dump_ok.then(|| match_stream(&stream, &recv)).unwrap_or(0);
        let loss_pct = dump_ok.then(|| 100.0 * (1.0 - match_bytes as f64 / stream.len() as f64));
        let row = pg::LossRow {
            t0: SystemTime::now(),
            sent_pkts: sent as i32,
            sent_bytes: stream.len() as i32,
            recv_bytes: recv.len() as i32,
            match_bytes: match_bytes as i32,
            loss_pct,
            dump_ok,
            dump_attempts: dump_attempts as i32,
            sent_sha256: None,
            recv_sha256: None,
        };
        let config_id = pg
            .ingest_until_ok(
                pps as i32,
                task,
                window.as_secs() as i32,
                seed as i64,
                &row,
                &stop,
            )
            .await;
        let stats = RoundStats {
            sent_pkts: sent,
            recv_bytes: recv.len(),
            match_bytes,
            loss_pct,
            dump_ok,
            dump_attempts,
            config_id,
        };
        eprintln!(
            "  → sent={} recv={} match={} loss={} dump_ok={} attempts={} pg={:?}",
            stats.sent_pkts,
            stats.recv_bytes,
            stats.match_bytes,
            stats
                .loss_pct
                .map(|v| format!("{v:.1}%"))
                .unwrap_or_else(|| "n/a".into()),
            stats.dump_ok,
            stats.dump_attempts,
            stats.config_id
        );
        harness.reset_state().await;
        done += 1;
        need_reboot = true;
    }
    eprintln!(
        "rate-loss done {done}/{total} elapsed {}",
        rate_common::fmt_dur(started.elapsed())
    );
    Ok(())
}

fn gen_stream(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..len).map(|_| rng.random_range(1u8..=255)).collect()
}

fn build_collect_pkt(index: u16, data: u8) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(UDP_PAYLOAD_LEN);
    pkt.extend_from_slice(&MAGIC_COLLECT);
    pkt.extend_from_slice(&index.to_le_bytes());
    pkt.push(data);
    pkt.resize(UDP_PAYLOAD_LEN.max(COLLECT_HDR_LEN), 0);
    pkt
}

fn match_stream(sent: &[u8], recv: &[u8]) -> usize {
    sent.iter()
        .zip(recv.iter().chain(std::iter::repeat(&0)))
        .filter(|(a, b)| a == b)
        .count()
}

async fn send_stream(
    harness: &Harness,
    src: std::net::Ipv4Addr,
    stream: &[u8],
    window: Duration,
    stop: &Stop,
) -> Result<usize> {
    let started = Instant::now();
    let deadline = started + window;
    let gap = Duration::from_micros(u64::from(LEGIT_INTERVAL_US));
    let mut next = started;
    let mut sent = 0;
    for (index, &byte) in stream.iter().enumerate() {
        if stop.is_set() || Instant::now() >= deadline {
            break;
        }
        let now = Instant::now();
        if now < next {
            stop.sleep_or(next - now).await;
        }
        if stop.is_set() || Instant::now() >= deadline {
            break;
        }
        harness
            .send_echo_payload(src, &build_collect_pkt(index as u16, byte))
            .context("send collect")?;
        sent += 1;
        next += gap;
        if sent % 32 == 0 {
            tokio::task::yield_now().await;
        }
    }
    Ok(sent)
}

async fn dump_stream(
    harness: &Harness,
    src: std::net::Ipv4Addr,
    stop: &Stop,
) -> (Vec<u8>, bool, u32) {
    for attempt in 1..=DUMP_RETRIES {
        if stop.is_set() {
            return (Vec::new(), false, attempt);
        }
        match harness.collect_dump(src, STREAM_LEN as u16).await {
            Ok(bytes) => return (bytes, true, attempt),
            Err(e) => {
                warn!("collect dump attempt {attempt}/{DUMP_RETRIES}: {e:#}");
                stop.sleep_or(DUMP_RETRY_GAP).await;
            }
        }
    }
    (Vec::new(), false, DUMP_RETRIES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_is_nonzero_and_repeatable() {
        assert_eq!(gen_stream(7, 32), gen_stream(7, 32));
        assert!(gen_stream(7, 32).iter().all(|byte| *byte != 0));
    }

    #[test]
    fn stream_match_counts_positions() {
        assert_eq!(match_stream(&[1, 2, 3], &[1, 0, 3]), 2);
        assert_eq!(match_stream(&[1, 2, 3], &[]), 0);
    }
}
