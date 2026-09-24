//! Self-built UDP flood for drop benches (64-byte L2 frames, 4 parallel senders).

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::raw_tx::RawTx;

pub const BLAST_THREADS: usize = 4;

#[derive(Debug, Clone, Copy)]
pub struct BlastResult {
    pub tx_start: Instant,
    pub tx_end: Instant,
    pub tx_count: u64,
}

impl BlastResult {
    pub fn elapsed_secs(&self) -> f64 {
        self.tx_end
            .duration_since(self.tx_start)
            .as_secs_f64()
            .max(1e-6)
    }

    pub fn host_pps(&self) -> f64 {
        self.tx_count as f64 / self.elapsed_secs()
    }
}

struct BlastHandle {
    stop: Arc<AtomicBool>,
    tx_count: Arc<AtomicU64>,
    threads: Vec<JoinHandle<()>>,
}

impl BlastHandle {
    fn start(
        raw: Arc<RawTx>,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        dport: u16,
        threads: usize,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let tx_count = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::with_capacity(threads);
        for _ in 0..threads {
            let raw = raw.clone();
            let stop = stop.clone();
            let tx_count = tx_count.clone();
            handles.push(thread::spawn(move || {
                blast_loop(raw, src_ip, dst_ip, dport, stop, tx_count)
            }));
        }
        Self {
            stop,
            tx_count,
            threads: handles,
        }
    }

    fn finish(self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
        self.tx_count.load(Ordering::Relaxed)
    }
}

fn blast_loop(
    raw: Arc<RawTx>,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    dport: u16,
    stop: Arc<AtomicBool>,
    tx_count: Arc<AtomicU64>,
) {
    let mut rng = rand::rng();
    while !stop.load(Ordering::Relaxed) {
        let frame = raw.build_udp_frame_64(src_ip, dst_ip, dport, &mut rng);
        if raw.send_udp_frame(&frame).is_ok() {
            tx_count.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Flood UDP until `stop` or `max_window`, then return host send metrics.
pub fn run_blast_until_stop(
    raw: Arc<RawTx>,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    dport: u16,
    stop: Arc<AtomicBool>,
    max_window: Duration,
) -> BlastResult {
    let tx_start = Instant::now();
    let blast = BlastHandle::start(raw, src_ip, dst_ip, dport, BLAST_THREADS);
    while !stop.load(Ordering::Acquire) && tx_start.elapsed() < max_window {
        thread::sleep(Duration::from_millis(1));
    }
    let tx_count = blast.finish();
    BlastResult {
        tx_start,
        tx_end: Instant::now(),
        tx_count,
    }
}

/// Flood UDP for `window`, then return host send metrics.
pub fn run_blast_window(
    raw: Arc<RawTx>,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    dport: u16,
    window: Duration,
) -> Result<BlastResult> {
    let tx_start = Instant::now();
    let blast = BlastHandle::start(raw, src_ip, dst_ip, dport, BLAST_THREADS);
    thread::sleep(window);
    let tx_count = blast.finish();
    Ok(BlastResult {
        tx_start,
        tx_end: Instant::now(),
        tx_count,
    })
}
