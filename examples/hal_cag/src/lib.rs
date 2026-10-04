//! Local CAG wire report for HAL firmwares (counter + aggregate + gauge).
//!
//! Layout matches `xpost::ext::perf` CAG (184 + 8×176 + 32 = 1624 bytes).
//! HAL fills the subset that maps to App UDP dataplane; XDP-only slots stay 0.

#![no_std]

use core::convert::Infallible;
use core::sync::atomic::Ordering;

use portable_atomic::{AtomicU32, AtomicU64, AtomicUsize};

/// Counter section length (46 × u32 LE).
pub const COUNTER_WIRE_LEN: usize = 184;
/// Aggregate section length (8 × 176).
pub const AGGREGATE_WIRE_LEN: usize = 8 * 176;
/// Gauge section length (4 × (current,peak) u32).
pub const GAUGE_WIRE_LEN: usize = 32;
/// Full CAG payload length (`perf-cag` / mask `0x7`).
pub const CAG_WIRE_LEN: usize = COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN + GAUGE_WIRE_LEN;

const HIST_BUCKETS: usize = 16;
const AGG_COUNT: usize = 8;

/// Aggregate slot indices (wire order = xpost encode_aggregate).
const AGG_RX_PROCESSING: usize = 0;
const AGG_EXECUTION: usize = 1; // wire name `xdp_execution` → DB `execution`
const AGG_DISPATCH: usize = 2; // wire name `xdp_dispatch` → DB `dispatch`
const AGG_POST_ACTION: usize = 3;
const AGG_TX_SUBMISSION: usize = 4;
const AGG_TX_BLOCK: usize = 5;
const AGG_HELPER: usize = 6;
const AGG_MAP: usize = 7;

struct Agg {
    count: AtomicU64,
    total_ticks: AtomicU64,
    min_ticks: AtomicU64,
    max_ticks: AtomicU64,
    lo: AtomicU64,
    width: AtomicU64,
    buckets: [AtomicU64; HIST_BUCKETS],
    ready: AtomicU32,
}

impl Agg {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            min_ticks: AtomicU64::new(u64::MAX),
            max_ticks: AtomicU64::new(0),
            lo: AtomicU64::new(0),
            width: AtomicU64::new(1),
            buckets: [const { AtomicU64::new(0) }; HIST_BUCKETS],
            ready: AtomicU32::new(0),
        }
    }

    fn record(&self, ticks: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ticks.fetch_add(ticks, Ordering::Relaxed);
        let _ = self
            .min_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (ticks < m).then_some(ticks)
            });
        let _ = self
            .max_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (ticks > m).then_some(ticks)
            });
        self.insert_hist(ticks);
    }

    fn insert_hist(&self, ticks: u64) {
        if self
            .ready
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            self.lo.store(ticks, Ordering::Relaxed);
            self.width.store(1, Ordering::Relaxed);
        }
        critical_section::with(|_| {
            loop {
                let lo = self.lo.load(Ordering::Relaxed);
                let width = self.width.load(Ordering::Relaxed).max(1);
                let hi = lo.saturating_add(width.saturating_mul(HIST_BUCKETS as u64));
                if ticks >= lo && ticks < hi {
                    let idx = ((ticks - lo) / width) as usize;
                    self.buckets[idx.min(HIST_BUCKETS - 1)].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                if ticks < lo {
                    self.expand_down(lo, width);
                } else {
                    self.expand_up(width);
                }
            }
        });
    }

    fn expand_up(&self, width: u64) {
        let mut merged = [0u64; HIST_BUCKETS];
        for i in 0..(HIST_BUCKETS / 2) {
            merged[i] = self.buckets[2 * i].load(Ordering::Relaxed)
                + self.buckets[2 * i + 1].load(Ordering::Relaxed);
        }
        for i in 0..HIST_BUCKETS {
            self.buckets[i].store(merged[i], Ordering::Relaxed);
        }
        self.width
            .store(width.saturating_mul(2).max(1), Ordering::Relaxed);
    }

    fn expand_down(&self, lo: u64, width: u64) {
        let half = HIST_BUCKETS / 2;
        let shift = (half as u64).saturating_mul(width);
        let new_lo = lo.saturating_sub(shift);
        for i in (0..half).rev() {
            let v = self.buckets[i].swap(0, Ordering::Relaxed);
            self.buckets[i + half].store(
                self.buckets[i + half].load(Ordering::Relaxed) + v,
                Ordering::Relaxed,
            );
        }
        self.lo.store(new_lo, Ordering::Relaxed);
        if new_lo == lo {
            self.expand_up(width);
            let w = self.width.load(Ordering::Relaxed).max(1);
            let cur_lo = self.lo.load(Ordering::Relaxed);
            if cur_lo > 0 {
                let half_span = (half as u64).saturating_mul(w);
                self.lo
                    .store(cur_lo.saturating_sub(half_span), Ordering::Relaxed);
                for i in (0..half).rev() {
                    let v = self.buckets[i].swap(0, Ordering::Relaxed);
                    self.buckets[i + half].store(
                        self.buckets[i + half].load(Ordering::Relaxed) + v,
                        Ordering::Relaxed,
                    );
                }
            }
        }
    }

    fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.total_ticks.store(0, Ordering::Relaxed);
        self.min_ticks.store(u64::MAX, Ordering::Relaxed);
        self.max_ticks.store(0, Ordering::Relaxed);
        self.lo.store(0, Ordering::Relaxed);
        self.width.store(1, Ordering::Relaxed);
        self.ready.store(0, Ordering::Relaxed);
        for b in &self.buckets {
            b.store(0, Ordering::Relaxed);
        }
    }

    fn encode(&self, out: &mut [u8]) {
        let min = self.min_ticks.load(Ordering::Relaxed);
        let vals = [
            self.count.load(Ordering::Relaxed),
            self.total_ticks.load(Ordering::Relaxed),
            if min == u64::MAX { 0 } else { min },
            self.max_ticks.load(Ordering::Relaxed),
            self.lo.load(Ordering::Relaxed),
            self.width.load(Ordering::Relaxed).max(1),
        ];
        let mut o = 0usize;
        for v in vals {
            out[o..o + 8].copy_from_slice(&v.to_le_bytes());
            o += 8;
        }
        for b in &self.buckets {
            out[o..o + 8].copy_from_slice(&b.load(Ordering::Relaxed).to_le_bytes());
            o += 8;
        }
    }
}

struct Gauge {
    current: AtomicUsize,
    peak: AtomicUsize,
}

impl Gauge {
    const fn new() -> Self {
        Self {
            current: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }
    }

    fn set(&self, v: usize) {
        self.current.store(v, Ordering::Relaxed);
        let _ = self
            .peak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (v > m).then_some(v)
            });
    }

    fn reset(&self) {
        self.current.store(0, Ordering::Relaxed);
        self.peak.store(0, Ordering::Relaxed);
    }
}

struct State {
    rx_packets: AtomicUsize,
    rx_delivered: AtomicUsize,
    rx_discarded: AtomicUsize,
    tx_submitted: AtomicUsize,
    tx_failed: AtomicUsize,
    tx_completed: AtomicUsize,
    aggs: [Agg; AGG_COUNT],
    rx_queue_depth: Gauge,
    tx_queue_depth: Gauge,
    tx_inflight: Gauge,
    trace_buffer_usage: Gauge,
}

impl State {
    const fn new() -> Self {
        Self {
            rx_packets: AtomicUsize::new(0),
            rx_delivered: AtomicUsize::new(0),
            rx_discarded: AtomicUsize::new(0),
            tx_submitted: AtomicUsize::new(0),
            tx_failed: AtomicUsize::new(0),
            tx_completed: AtomicUsize::new(0),
            aggs: [
                Agg::new(),
                Agg::new(),
                Agg::new(),
                Agg::new(),
                Agg::new(),
                Agg::new(),
                Agg::new(),
                Agg::new(),
            ],
            rx_queue_depth: Gauge::new(),
            tx_queue_depth: Gauge::new(),
            tx_inflight: Gauge::new(),
            trace_buffer_usage: Gauge::new(),
        }
    }

    fn reset(&self) {
        self.rx_packets.store(0, Ordering::Relaxed);
        self.rx_delivered.store(0, Ordering::Relaxed);
        self.rx_discarded.store(0, Ordering::Relaxed);
        self.tx_submitted.store(0, Ordering::Relaxed);
        self.tx_failed.store(0, Ordering::Relaxed);
        self.tx_completed.store(0, Ordering::Relaxed);
        for a in &self.aggs {
            a.reset();
        }
        self.rx_queue_depth.reset();
        self.tx_queue_depth.reset();
        self.tx_inflight.reset();
        self.trace_buffer_usage.reset();
    }
}

static STATE: State = State::new();

/// Monotonic ticks (µs since epoch), same unit as xpost aggregates.
#[inline]
pub fn now_ticks() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

#[inline]
fn bump(c: &AtomicUsize) {
    c.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub fn bump_rx_packets() {
    bump(&STATE.rx_packets);
}

#[inline]
pub fn bump_rx_delivered() {
    bump(&STATE.rx_delivered);
}

#[inline]
pub fn bump_rx_discarded() {
    bump(&STATE.rx_discarded);
}

#[inline]
pub fn bump_tx_submitted() {
    bump(&STATE.tx_submitted);
}

#[inline]
pub fn bump_tx_failed() {
    bump(&STATE.tx_failed);
}

#[inline]
pub fn bump_tx_completed() {
    bump(&STATE.tx_completed);
}

#[inline]
pub fn record_rx_processing(ticks: u64) {
    STATE.aggs[AGG_RX_PROCESSING].record(ticks);
}

#[inline]
pub fn record_execution(ticks: u64) {
    STATE.aggs[AGG_EXECUTION].record(ticks);
}

#[inline]
pub fn record_dispatch(ticks: u64) {
    STATE.aggs[AGG_DISPATCH].record(ticks);
}

#[inline]
pub fn record_post_action(ticks: u64) {
    STATE.aggs[AGG_POST_ACTION].record(ticks);
}

#[inline]
pub fn record_tx_submission(ticks: u64) {
    STATE.aggs[AGG_TX_SUBMISSION].record(ticks);
}

#[inline]
pub fn record_map_operation(ticks: u64) {
    STATE.aggs[AGG_MAP].record(ticks);
}

#[inline]
pub fn set_rx_queue_depth(v: usize) {
    STATE.rx_queue_depth.set(v);
}

#[inline]
pub fn set_tx_inflight(v: usize) {
    STATE.tx_inflight.set(v);
    STATE.tx_queue_depth.set(v);
}

/// Reset every CAG accumulator.
pub fn clear_perf() {
    STATE.reset();
}

/// Payload length for the compiled CAG layout.
pub const fn report_payload_len() -> usize {
    CAG_WIRE_LEN
}

/// Stream the CAG report through `write` (raw LE bytes, no framing).
pub fn report_perf(mut write: impl FnMut(&[u8]) -> Result<(), Infallible>) {
    let mut buf = [0u8; CAG_WIRE_LEN];
    encode_counter(&mut buf[..COUNTER_WIRE_LEN]);
    encode_aggregate(&mut buf[COUNTER_WIRE_LEN..COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN]);
    encode_gauge(&mut buf[COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN..]);
    let _ = write(&buf);
}

fn encode_counter(out: &mut [u8]) {
    debug_assert_eq!(out.len(), COUNTER_WIRE_LEN);
    let mut o = 0usize;
    let put = |v: u32, out: &mut [u8], o: &mut usize| {
        out[*o..*o + 4].copy_from_slice(&v.to_le_bytes());
        *o += 4;
    };
    let put_usz = |v: usize, out: &mut [u8], o: &mut usize| {
        put(v as u32, out, o);
    };
    // Must match xpost `encode_counter` / host COUNTER_NAMES order.
    put_usz(STATE.rx_packets.load(Ordering::Relaxed), out, &mut o);
    put_usz(0, out, &mut o); // xdp_invocations
    put_usz(0, out, &mut o); // xdp_pass
    put_usz(0, out, &mut o); // xdp_drop
    put_usz(0, out, &mut o); // xdp_tx
    put_usz(0, out, &mut o); // xdp_aborted
    put_usz(STATE.rx_delivered.load(Ordering::Relaxed), out, &mut o);
    put_usz(STATE.rx_discarded.load(Ordering::Relaxed), out, &mut o);
    put_usz(0, out, &mut o); // rx_queue_overflow
    put_usz(STATE.tx_submitted.load(Ordering::Relaxed), out, &mut o);
    put_usz(STATE.tx_failed.load(Ordering::Relaxed), out, &mut o);
    put_usz(STATE.tx_completed.load(Ordering::Relaxed), out, &mut o);
    for _ in 0..(46 - 12) {
        put_usz(0, out, &mut o);
    }
    debug_assert_eq!(o, COUNTER_WIRE_LEN);
}

fn encode_aggregate(out: &mut [u8]) {
    debug_assert_eq!(out.len(), AGGREGATE_WIRE_LEN);
    for (i, agg) in STATE.aggs.iter().enumerate() {
        let off = i * 176;
        agg.encode(&mut out[off..off + 176]);
    }
    let _ = (AGG_TX_BLOCK, AGG_HELPER); // slots reserved / stay zero unless hooked
}

fn encode_gauge(out: &mut [u8]) {
    debug_assert_eq!(out.len(), GAUGE_WIRE_LEN);
    let vals = [
        STATE.rx_queue_depth.current.load(Ordering::Relaxed) as u32,
        STATE.rx_queue_depth.peak.load(Ordering::Relaxed) as u32,
        STATE.tx_queue_depth.current.load(Ordering::Relaxed) as u32,
        STATE.tx_queue_depth.peak.load(Ordering::Relaxed) as u32,
        STATE.tx_inflight.current.load(Ordering::Relaxed) as u32,
        STATE.tx_inflight.peak.load(Ordering::Relaxed) as u32,
        STATE.trace_buffer_usage.current.load(Ordering::Relaxed) as u32,
        STATE.trace_buffer_usage.peak.load(Ordering::Relaxed) as u32,
    ];
    for (i, v) in vals.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}
