//! HAL dataplane performance + App serial map wire helpers (no `xpost`).
//!
//! # Wire layout
//!
//! CAGT payload matches `xpost::ext::perf` / host decode
//! (`184 + 11×176 + 32 + 5136 = 7288` bytes). Counter, Aggregate, Gauge, and
//! Trace are always present. Firmwares call the bump/record/push APIs they can
//! fill. Unhooked slots stay **0** on the wire.
//!
//! # Counter catalog (46 × u32 LE)
//!
//! | wire name | meaning | HAL cut-in |
//! |---|---|---|
//! | `rx_packets` | frames / UDP datagrams seen at the App stim socket | `recv_from` ok |
//! | `xdp_invocations` | handler entries (XDP lease analog) | top of UDP loop body |
//! | `xdp_pass` | policy Allow / deliver-to-stack | pass / ACK path |
//! | `xdp_drop` | policy Drop (discard, no ACK) | `DROP_ARMED` / blacklist hit |
//! | `xdp_tx` | policy Tx / App echo-TX | just_tx send path |
//! | `xdp_aborted` | handler aborted / unhandled error | parse fail, bind errors |
//! | `rx_delivered` | delivered upward (Pass) | before successful ACK/echo |
//! | `rx_discarded` | discarded (Drop) | Drop branch |
//! | `rx_queue_overflow` | RX drop due to full queue / no buffer | `recv` err / overflow |
//! | `tx_submitted` | TX attempts started | before `send_to` |
//! | `tx_failed` | TX submit/send failed | `send_to` Err |
//! | `tx_completed` | TX completed ok | `send_to` Ok |
//! | `runtime_errors` | soft runtime faults | unexpected Err paths |
//! | `tx_fail_bad_buf` | TX bad buffer (Wi-Fi stack) | if esp-radio exposes |
//! | `tx_fail_token` | TX token fail | if esp-radio exposes |
//! | `tx_grp_src` | TX group/source accounting | reserved / 0 |
//! | `tx_enq` | TX enqueue | if queue API exposed |
//! | `tx_enq_fail_claim` | enqueue claim fail | reserved / 0 |
//! | `tx_enq_fail_ring` | enqueue ring fail | reserved / 0 |
//! | `tx_kick` | TX kick/doorbell | reserved / 0 |
//! | `tx_poll` | TX poll/completion poll | reserved / 0 |
//! | `tx_cap_wait` | waited on TX capacity | long `send_to` / block |
//! | `tx_nomem_wait` | waited on TX mem | OOM wait if observed |
//! | `tx_no_retain` | no-retain TX path | reserved / 0 |
//! | `tx_flush` | TX flush | reserved / 0 |
//! | `tx_air_done_fail` | air-done failure | reserved / 0 |
//! | `tx_ns_ref` | TX ns ref | reserved / 0 |
//! | `tx_ns_free` | TX ns free | reserved / 0 |
//! | `tx_flow_sample` | flow sample tick | optional sampling |
//! | `tx_block_gt10ms` | TX blocked >10ms | `note_block_end` |
//! | `tx_block_gt100ms` | TX blocked >100ms | `note_block_end` |
//! | `inactive_skip` | skipped while inactive | XDP-off path |
//! | `reentry` | re-entrant handler | nested recv (rare) |
//! | `stack_misalign` | stack misalign | reserved / 0 |
//! | `bad_md` | bad metadata | bad endpoint / key |
//! | `helper_unresolved` | helper unresolved | reserved / 0 |
//! | `helper_map` | map-like helper touch count | ARRAY/HASH lookup+update |
//! | `pool_drop_misalign` | pool drop misalign | reserved / 0 |
//! | `tx_wifi_last_rc` | last Wi-Fi TX rc (i32 bits) | `set_tx_wifi_last_rc` |
//! | `tx_pend` | pending TX depth snapshot | `set_tx_pend` |
//! | `tx_inflight` | inflight TX depth snapshot | `set_tx_inflight` / gauge |
//! | `tx_inflight_cap` | inflight capacity | `set_tx_inflight_cap` |
//! | `tx_pending_peak` | peak pending | auto via `set_tx_pend` |
//! | `tx_inflight_peak` | peak inflight | auto via inflight gauge |
//! | `tx_block_max_us` | max TX block µs | `note_block_end` |
//! | `block_since_us` | internal block start (export 0) | always 0 on wire |
//!
//! # Aggregate catalog (8 filled slots + 3 zero lookup slots, 176 B each)
//!
//! | wire / DB name | meaning | HAL cut-in |
//! |---|---|---|
//! | `rx_processing` | full stim handling (recv→decision→TX/drop) | `t0`…end of iteration |
//! | `xdp_execution` → `execution` | policy / map work body | map+decision window |
//! | `xdp_dispatch` → `dispatch` | dispatch / pre-policy | recv→start of exec |
//! | `post_action` | after decision (TX/ACK) | `send_to` window |
//! | `tx_submission` | TX submit latency | same or nested in post_action |
//! | `tx_block` | blocked wait inside TX | `note_block_start/end` |
//! | `helper_execution` | generic helper body | non-map helper analog |
//! | `map_operation` | ARRAY/HASH lookup+inc/update | around counter/blacklist |
//!
//! Slots after `map_operation` are the XDP lookup histograms
//! (`lookup_non_null`, `lookup_null`, `lookup_timer_empty`).
//! HAL writes them empty. `lo = 1` and `width = 0` mark lookup edge version 1.
//!
//! # Gauge catalog (4 × current+peak u32)
//!
//! | name | meaning | HAL cut-in |
//! |---|---|---|
//! | `rx_queue_depth` | in-flight RX work items | 1 on enter, 0 on leave |
//! | `tx_queue_depth` | TX queue depth | mirror inflight or 0 |
//! | `tx_inflight` | outstanding TX | 1 around `send_to` |
//! | `trace_buffer_usage` | Trace radio ring fill | `push_trace` |

#![no_std]

use core::cell::RefCell;
use core::convert::Infallible;
use core::sync::atomic::Ordering;

use critical_section::Mutex;
use heapless::Deque;
use portable_atomic::{AtomicI32, AtomicU32, AtomicU64, AtomicUsize};

/// Counter section length (46 × u32 LE).
pub const COUNTER_WIRE_LEN: usize = 184;
/// Aggregate section length (8 HAL slots + 3 XDP lookup slots).
pub const AGGREGATE_WIRE_LEN: usize = 11 * 176;
/// Gauge section length (4 × (current,peak) u32).
pub const GAUGE_WIRE_LEN: usize = 32;
/// Radio / XDP Trace ring capacity (matches xpost).
pub const TRACE_CAP: usize = 256;
/// App Trace ring capacity (reserved; HAL writers use the radio ring).
pub const TRACE_APP_CAP: usize = 64;
/// Trace section: hdr(16) + radio[CAP] + app[APP_CAP].
pub const TRACE_WIRE_LEN: usize = 16 + TRACE_CAP * 16 + TRACE_APP_CAP * 16;
/// CAG payload length (mask `0x7` fragment).
pub const CAG_WIRE_LEN: usize = COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN + GAUGE_WIRE_LEN;
/// Full CAGT payload (mask `0xf`).
pub const CAGT_WIRE_LEN: usize = CAG_WIRE_LEN + TRACE_WIRE_LEN;

/// Wire `kind` for `xdp_begin`.
pub const KIND_XDP_BEGIN: u8 = 2;
/// Wire `kind` for `xdp_end`.
pub const KIND_XDP_END: u8 = 3;
/// Wire `kind` for `helper_enter`.
pub const KIND_HELPER_ENTER: u8 = 11;
/// Wire `kind` for `helper_leave`.
pub const KIND_HELPER_LEAVE: u8 = 12;
/// `arg0` on `xdp_end` for XDP_PASS.
pub const TRACE_XDP_PASS: u32 = 2;
/// `arg0` on helper events for `bpf_trace_printk` / UART finish twin.
pub const TRACE_HELPER_PRINTK: u32 = 6;

const HIST_BUCKETS: usize = 16;
const AGG_COUNT: usize = 8;
const AGG_SLOTS: usize = 11;
const COUNTER_COUNT: usize = 46;

// Counter wire indices (must match host `COUNTER_NAMES` / xpost encode).
const C_RX_PACKETS: usize = 0;
const C_XDP_INVOCATIONS: usize = 1;
const C_XDP_PASS: usize = 2;
const C_XDP_DROP: usize = 3;
const C_XDP_TX: usize = 4;
const C_XDP_ABORTED: usize = 5;
const C_RX_DELIVERED: usize = 6;
const C_RX_DISCARDED: usize = 7;
const C_RX_QUEUE_OVERFLOW: usize = 8;
const C_TX_SUBMITTED: usize = 9;
const C_TX_FAILED: usize = 10;
const C_TX_COMPLETED: usize = 11;
const C_RUNTIME_ERRORS: usize = 12;
const C_TX_FAIL_BAD_BUF: usize = 13;
const C_TX_FAIL_TOKEN: usize = 14;
const C_TX_GRP_SRC: usize = 15;
const C_TX_ENQ: usize = 16;
const C_TX_ENQ_FAIL_CLAIM: usize = 17;
const C_TX_ENQ_FAIL_RING: usize = 18;
const C_TX_KICK: usize = 19;
const C_TX_POLL: usize = 20;
const C_TX_CAP_WAIT: usize = 21;
const C_TX_NOMEM_WAIT: usize = 22;
const C_TX_NO_RETAIN: usize = 23;
const C_TX_FLUSH: usize = 24;
const C_TX_AIR_DONE_FAIL: usize = 25;
const C_TX_NS_REF: usize = 26;
const C_TX_NS_FREE: usize = 27;
const C_TX_FLOW_SAMPLE: usize = 28;
const C_TX_BLOCK_GT10MS: usize = 29;
const C_TX_BLOCK_GT100MS: usize = 30;
const C_INACTIVE_SKIP: usize = 31;
const C_REENTRY: usize = 32;
const C_STACK_MISALIGN: usize = 33;
const C_BAD_MD: usize = 34;
const C_HELPER_UNRESOLVED: usize = 35;
const C_HELPER_MAP: usize = 36;
const C_POOL_DROP_MISALIGN: usize = 37;
// 38 = tx_wifi_last_rc (i32, separate store)
const C_TX_PEND: usize = 39;
const C_TX_INFLIGHT: usize = 40;
const C_TX_INFLIGHT_CAP: usize = 41;
const C_TX_PENDING_PEAK: usize = 42;
const C_TX_INFLIGHT_PEAK: usize = 43;
const C_TX_BLOCK_MAX_US: usize = 44;
// 45 = block_since_us → always 0 on wire

const AGG_RX_PROCESSING: usize = 0;
const AGG_EXECUTION: usize = 1;
const AGG_DISPATCH: usize = 2;
const AGG_POST_ACTION: usize = 3;
const AGG_TX_SUBMISSION: usize = 4;
const AGG_TX_BLOCK: usize = 5;
const AGG_HELPER: usize = 6;
const AGG_MAP: usize = 7;

macro_rules! bump_api {
    ($(($fn:ident, $idx:expr)),+ $(,)?) => {
        $(
            #[inline]
            pub fn $fn() {
                STATE.counters[$idx].fetch_add(1, Ordering::Relaxed);
            }
        )+
    };
}

macro_rules! record_api {
    ($(($fn:ident, $idx:expr)),+ $(,)?) => {
        $(
            #[inline]
            pub fn $fn(ticks: u64) {
                STATE.aggs[$idx].record(ticks);
            }
        )+
    };
}

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
        // Same window grow as `xpost::ext::perf::aggregate`.
        critical_section::with(|_| {
            if self.ready.load(Ordering::Relaxed) == 0 {
                self.lo.store(ticks, Ordering::Relaxed);
                self.width.store(1, Ordering::Relaxed);
                for b in &self.buckets {
                    b.store(0, Ordering::Relaxed);
                }
                self.ready.store(1, Ordering::Relaxed);
            }
            for _ in 0..96 {
                let lo = self.lo.load(Ordering::Relaxed);
                let width = self.width.load(Ordering::Relaxed).max(1);
                if ticks >= lo {
                    let idx = (ticks - lo) / width;
                    if idx < HIST_BUCKETS as u64 {
                        self.buckets[idx as usize].fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
                if !self.grow(ticks) {
                    break;
                }
            }
            let lo = self.lo.load(Ordering::Relaxed);
            let idx = if ticks < lo { 0 } else { HIST_BUCKETS - 1 };
            self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        });
    }

    /// Double `width` and slide `lo` on that grid. See xpost `Aggregate::grow`.
    fn grow(&self, ticks: u64) -> bool {
        let width = self.width.load(Ordering::Relaxed).max(1);
        let Some(new_width) = width.checked_mul(2) else {
            return false;
        };
        let lo = self.lo.load(Ordering::Relaxed);
        debug_assert_eq!(lo % width, 0);
        let unit = lo / width;
        let parity = (unit % 2) as usize;
        let max_shift = (unit as usize).min(HIST_BUCKETS);
        let max_aligned = if max_shift % 2 == parity {
            max_shift
        } else {
            max_shift.saturating_sub(1)
        };
        let shift = if ticks < lo {
            let need = ((lo - ticks + width - 1) / width) as usize;
            let need_aligned = if need % 2 == parity { need } else { need + 1 };
            if need_aligned <= max_aligned {
                need_aligned
            } else {
                max_aligned
            }
        } else if parity == 0 || max_aligned == 0 {
            0
        } else {
            1
        };
        if ticks < lo && shift == 0 {
            return false;
        }
        let mut merged = [0u64; HIST_BUCKETS];
        for i in 0..HIST_BUCKETS {
            let dst = (shift + i) / 2;
            debug_assert!(dst < HIST_BUCKETS);
            merged[dst] = merged[dst].saturating_add(self.buckets[i].load(Ordering::Relaxed));
        }
        for (i, count) in merged.iter().enumerate() {
            self.buckets[i].store(*count, Ordering::Relaxed);
        }
        self.lo
            .store(lo - (shift as u64) * width, Ordering::Relaxed);
        self.width.store(new_width, Ordering::Relaxed);
        true
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
    counters: [AtomicUsize; COUNTER_COUNT],
    tx_wifi_last_rc: AtomicI32,
    block_since_us: AtomicU32,
    aggs: [Agg; AGG_COUNT],
    rx_queue_depth: Gauge,
    tx_queue_depth: Gauge,
    tx_inflight: Gauge,
    trace_buffer_usage: Gauge,
    radio: Mutex<RefCell<Deque<Event, TRACE_CAP>>>,
    radio_dropped: AtomicUsize,
}

#[derive(Clone, Copy)]
struct Event {
    kind: u8,
    arg0: u32,
    arg1: u32,
    t_us: u32,
}

impl State {
    const fn new() -> Self {
        Self {
            counters: [const { AtomicUsize::new(0) }; COUNTER_COUNT],
            tx_wifi_last_rc: AtomicI32::new(0),
            block_since_us: AtomicU32::new(0),
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
            radio: Mutex::new(RefCell::new(Deque::new())),
            radio_dropped: AtomicUsize::new(0),
        }
    }

    fn reset(&self) {
        for c in &self.counters {
            c.store(0, Ordering::Relaxed);
        }
        self.tx_wifi_last_rc.store(0, Ordering::Relaxed);
        self.block_since_us.store(0, Ordering::Relaxed);
        for a in &self.aggs {
            a.reset();
        }
        self.rx_queue_depth.reset();
        self.tx_queue_depth.reset();
        self.tx_inflight.reset();
        self.trace_buffer_usage.reset();
        self.radio_dropped.store(0, Ordering::Relaxed);
        critical_section::with(|cs| {
            self.radio.borrow(cs).borrow_mut().clear();
        });
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

bump_api!(
    (bump_rx_packets, C_RX_PACKETS),
    (bump_xdp_invocations, C_XDP_INVOCATIONS),
    (bump_xdp_pass, C_XDP_PASS),
    (bump_xdp_drop, C_XDP_DROP),
    (bump_xdp_tx, C_XDP_TX),
    (bump_xdp_aborted, C_XDP_ABORTED),
    (bump_rx_delivered, C_RX_DELIVERED),
    (bump_rx_discarded, C_RX_DISCARDED),
    (bump_rx_queue_overflow, C_RX_QUEUE_OVERFLOW),
    (bump_tx_submitted, C_TX_SUBMITTED),
    (bump_tx_failed, C_TX_FAILED),
    (bump_tx_completed, C_TX_COMPLETED),
    (bump_runtime_errors, C_RUNTIME_ERRORS),
    (bump_tx_fail_bad_buf, C_TX_FAIL_BAD_BUF),
    (bump_tx_fail_token, C_TX_FAIL_TOKEN),
    (bump_tx_grp_src, C_TX_GRP_SRC),
    (bump_tx_enq, C_TX_ENQ),
    (bump_tx_enq_fail_claim, C_TX_ENQ_FAIL_CLAIM),
    (bump_tx_enq_fail_ring, C_TX_ENQ_FAIL_RING),
    (bump_tx_kick, C_TX_KICK),
    (bump_tx_poll, C_TX_POLL),
    (bump_tx_cap_wait, C_TX_CAP_WAIT),
    (bump_tx_nomem_wait, C_TX_NOMEM_WAIT),
    (bump_tx_no_retain, C_TX_NO_RETAIN),
    (bump_tx_flush, C_TX_FLUSH),
    (bump_tx_air_done_fail, C_TX_AIR_DONE_FAIL),
    (bump_tx_ns_ref, C_TX_NS_REF),
    (bump_tx_ns_free, C_TX_NS_FREE),
    (bump_tx_flow_sample, C_TX_FLOW_SAMPLE),
    (bump_tx_block_gt10ms, C_TX_BLOCK_GT10MS),
    (bump_tx_block_gt100ms, C_TX_BLOCK_GT100MS),
    (bump_inactive_skip, C_INACTIVE_SKIP),
    (bump_reentry, C_REENTRY),
    (bump_stack_misalign, C_STACK_MISALIGN),
    (bump_bad_md, C_BAD_MD),
    (bump_helper_unresolved, C_HELPER_UNRESOLVED),
    (bump_helper_map, C_HELPER_MAP),
    (bump_pool_drop_misalign, C_POOL_DROP_MISALIGN),
);

record_api!(
    (record_rx_processing, AGG_RX_PROCESSING),
    (record_execution, AGG_EXECUTION),
    (record_dispatch, AGG_DISPATCH),
    (record_post_action, AGG_POST_ACTION),
    (record_tx_submission, AGG_TX_SUBMISSION),
    (record_tx_block, AGG_TX_BLOCK),
    (record_helper_execution, AGG_HELPER),
    (record_map_operation, AGG_MAP),
);

#[inline]
pub fn set_tx_wifi_last_rc(rc: i32) {
    STATE.tx_wifi_last_rc.store(rc, Ordering::Relaxed);
}

#[inline]
pub fn set_tx_pend(v: usize) {
    STATE.counters[C_TX_PEND].store(v, Ordering::Relaxed);
    let _ = STATE.counters[C_TX_PENDING_PEAK].fetch_update(
        Ordering::Relaxed,
        Ordering::Relaxed,
        |m| (v > m).then_some(v),
    );
}

#[inline]
pub fn set_tx_inflight_cap(v: usize) {
    STATE.counters[C_TX_INFLIGHT_CAP].store(v, Ordering::Relaxed);
}

#[inline]
pub fn set_rx_queue_depth(v: usize) {
    STATE.rx_queue_depth.set(v);
}

#[inline]
pub fn set_tx_queue_depth(v: usize) {
    STATE.tx_queue_depth.set(v);
}

#[inline]
pub fn set_tx_inflight(v: usize) {
    STATE.tx_inflight.set(v);
    STATE.tx_queue_depth.set(v);
    STATE.counters[C_TX_INFLIGHT].store(v, Ordering::Relaxed);
    let peak = STATE.tx_inflight.peak.load(Ordering::Relaxed);
    STATE.counters[C_TX_INFLIGHT_PEAK].store(peak, Ordering::Relaxed);
}

#[inline]
pub fn set_trace_buffer_usage(v: usize) {
    STATE.trace_buffer_usage.set(v);
}

/// Mark start of a TX block/wait interval (µs).
#[inline]
pub fn note_block_start() {
    let t = now_ticks() as u32;
    let _ = STATE.block_since_us.compare_exchange(
        0,
        t.max(1),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

/// End TX block; records `tx_block` agg + gt10/100ms counters + max µs.
#[inline]
pub fn note_block_end() -> Option<u32> {
    let since = STATE.block_since_us.swap(0, Ordering::Relaxed);
    if since == 0 {
        return None;
    }
    let now = now_ticks() as u32;
    let dur = now.wrapping_sub(since);
    let _ = STATE.counters[C_TX_BLOCK_MAX_US].fetch_update(
        Ordering::Relaxed,
        Ordering::Relaxed,
        |m| (dur as usize > m).then_some(dur as usize),
    );
    if dur > 10_000 {
        bump_tx_block_gt10ms();
    }
    if dur > 100_000 {
        bump_tx_block_gt100ms();
    }
    record_tx_block(u64::from(dur));
    Some(dur)
}

/// Reset every CAG and Trace accumulator.
pub fn clear_perf() {
    STATE.reset();
}

/// Push one radio-ring event (newest-wins overwrite).
pub fn push_trace(kind: u8, arg0: u32, arg1: u32) {
    let ev = Event {
        kind,
        arg0,
        arg1,
        t_us: now_ticks() as u32,
    };
    let n = critical_section::with(|cs| {
        let mut q = STATE.radio.borrow(cs).borrow_mut();
        if q.is_full() {
            let _ = q.pop_front();
            STATE.radio_dropped.fetch_add(1, Ordering::Relaxed);
        }
        let _ = q.push_back(ev);
        q.len()
    });
    STATE.trace_buffer_usage.set(n);
}

/// Trace `xdp_begin`.
#[inline]
pub fn push_xdp_begin() {
    push_trace(KIND_XDP_BEGIN, 0, 0);
}

/// Trace `xdp_end` (`arg0` = XDP action).
#[inline]
pub fn push_xdp_end(action: u32) {
    push_trace(KIND_XDP_END, action, 0);
}

/// Trace `helper_enter` (`arg0` = helper id).
#[inline]
pub fn push_helper_enter(helper_id: u32) {
    push_trace(KIND_HELPER_ENTER, helper_id, 0);
}

/// Trace `helper_leave` (`arg0` = helper id).
#[inline]
pub fn push_helper_leave(helper_id: u32) {
    push_trace(KIND_HELPER_LEAVE, helper_id, 0);
}

/// Payload length for the compiled CAGT layout.
pub const fn report_payload_len() -> usize {
    CAGT_WIRE_LEN
}

/// Stream the CAGT report through `write` (raw LE bytes, no framing).
pub fn report_perf(mut write: impl FnMut(&[u8]) -> Result<(), Infallible>) {
    let mut buf = [0u8; CAGT_WIRE_LEN];
    encode_counter(&mut buf[..COUNTER_WIRE_LEN]);
    encode_aggregate(&mut buf[COUNTER_WIRE_LEN..COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN]);
    encode_gauge(
        &mut buf[COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN..COUNTER_WIRE_LEN + AGGREGATE_WIRE_LEN + GAUGE_WIRE_LEN],
    );
    encode_trace(&mut buf[CAG_WIRE_LEN..]);
    let _ = write(&buf);
}

fn encode_counter(out: &mut [u8]) {
    debug_assert_eq!(out.len(), COUNTER_WIRE_LEN);
    let mut o = 0usize;
    let put_u32 = |v: u32, out: &mut [u8], o: &mut usize| {
        out[*o..*o + 4].copy_from_slice(&v.to_le_bytes());
        *o += 4;
    };
    for i in 0..COUNTER_COUNT {
        if i == 38 {
            put_u32(
                STATE.tx_wifi_last_rc.load(Ordering::Relaxed) as u32,
                out,
                &mut o,
            );
        } else if i == 45 {
            // block_since_us is internal; export 0 (match xpost).
            put_u32(0, out, &mut o);
        } else {
            put_u32(
                STATE.counters[i].load(Ordering::Relaxed) as u32,
                out,
                &mut o,
            );
        }
    }
    debug_assert_eq!(o, COUNTER_WIRE_LEN);
}

fn encode_aggregate(out: &mut [u8]) {
    debug_assert_eq!(out.len(), AGGREGATE_WIRE_LEN);
    for (i, agg) in STATE.aggs.iter().enumerate() {
        let off = i * 176;
        agg.encode(&mut out[off..off + 176]);
    }
    // XDP lookup histograms. HAL leaves them empty. lo = edge version, width = 0.
    for i in AGG_COUNT..AGG_SLOTS {
        let o = i * 176 + 32;
        out[o..o + 8].copy_from_slice(&1u64.to_le_bytes());
    }
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

fn encode_trace(out: &mut [u8]) {
    debug_assert_eq!(out.len(), TRACE_WIRE_LEN);
    let mut radio = [Event {
        kind: 0,
        arg0: 0,
        arg1: 0,
        t_us: 0,
    }; TRACE_CAP];
    let (nr, dropped) = critical_section::with(|cs| {
        let q = STATE.radio.borrow(cs).borrow();
        let n = q.len();
        for (i, ev) in q.iter().enumerate() {
            radio[i] = *ev;
        }
        (n, STATE.radio_dropped.load(Ordering::Relaxed))
    });
    out[0..4].copy_from_slice(&(nr as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(dropped as u32).to_le_bytes());
    out[8..12].copy_from_slice(&0u32.to_le_bytes());
    out[12..16].copy_from_slice(&0u32.to_le_bytes());
    let radio_base = 16;
    for (i, ev) in radio.iter().enumerate() {
        let off = radio_base + i * 16;
        out[off] = ev.kind;
        out[off + 1] = 0;
        out[off + 2] = 0;
        out[off + 3] = 0;
        out[off + 4..off + 8].copy_from_slice(&ev.arg0.to_le_bytes());
        out[off + 8..off + 12].copy_from_slice(&ev.arg1.to_le_bytes());
        out[off + 12..off + 16].copy_from_slice(&ev.t_us.to_le_bytes());
    }
}

/// ASCII map dump + token parse for App-mode serial (no xpost dependency).
pub mod wire {
    /// Decimal digit count for `n` (at least 1).
    pub fn dec_len(mut n: u64) -> usize {
        let mut len = 1usize;
        while n >= 10 {
            n /= 10;
            len += 1;
        }
        len
    }

    fn write_dec(mut n: u64, write: &mut impl FnMut(&[u8])) {
        let mut buf = [0u8; 20];
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        write(&buf[i..]);
    }

    /// Format a HASH key as a wire token (dotted IPv4 or hex).
    pub fn format_hash_key<'a>(key: &[u8], scratch: &'a mut [u8; 64]) -> &'a [u8] {
        if key.len() == 4 {
            let mut i = 0usize;
            for (idx, b) in key.iter().enumerate() {
                if idx > 0 {
                    scratch[i] = b'.';
                    i += 1;
                }
                let mut tmp = [0u8; 3];
                let mut n = *b as u64;
                let mut t = 3;
                loop {
                    t -= 1;
                    tmp[t] = b'0' + (n % 10) as u8;
                    n /= 10;
                    if n == 0 {
                        break;
                    }
                }
                let digs = &tmp[t..];
                scratch[i..i + digs.len()].copy_from_slice(digs);
                i += digs.len();
            }
            &scratch[..i]
        } else {
            const HEX: &[u8] = b"0123456789abcdef";
            let n = key.len().min(32);
            for (i, b) in key.iter().take(n).enumerate() {
                scratch[i * 2] = HEX[(b >> 4) as usize];
                scratch[i * 2 + 1] = HEX[(b & 0xf) as usize];
            }
            &scratch[..n * 2]
        }
    }

    /// Interpret map value bytes as a little-endian integer (≤ 8 bytes).
    pub fn value_as_u64(value: &[u8]) -> u64 {
        let mut buf = [0u8; 8];
        let n = value.len().min(8);
        buf[..n].copy_from_slice(&value[..n]);
        u64::from_le_bytes(buf)
    }

    /// Header + entries length for an ARRAY snapshot.
    pub fn array_blob_len(size: u32, mut value_at: impl FnMut(u32) -> u64) -> usize {
        let mut len = 6 + dec_len(u64::from(size)) + 1;
        for i in 0..size {
            let v = value_at(i);
            len += dec_len(u64::from(i)) + 1 + dec_len(v) + 1;
        }
        len
    }

    /// Write an ARRAY snapshot blob.
    pub fn write_array_blob(
        size: u32,
        mut value_at: impl FnMut(u32) -> u64,
        write: &mut impl FnMut(&[u8]),
    ) {
        write(b"array ");
        write_dec(u64::from(size), write);
        write(b"\n");
        for i in 0..size {
            write_dec(u64::from(i), write);
            write(b" ");
            write_dec(value_at(i), write);
            write(b"\n");
        }
    }

    /// Length of a HASH header line `hash <size> <cap>\n`.
    pub fn hash_header_len(size: u32, cap: u32) -> usize {
        5 + dec_len(u64::from(size)) + 1 + dec_len(u64::from(cap)) + 1
    }

    /// Write a HASH header line.
    pub fn write_hash_header(size: u32, cap: u32, write: &mut impl FnMut(&[u8])) {
        write(b"hash ");
        write_dec(u64::from(size), write);
        write(b" ");
        write_dec(u64::from(cap), write);
        write(b"\n");
    }

    /// Length of one HASH entry line.
    pub fn hash_entry_len(key: &[u8], value: &[u8]) -> usize {
        let mut scratch = [0u8; 64];
        let key_tok = format_hash_key(key, &mut scratch);
        key_tok.len() + 1 + format_hash_value_len(value) + 1
    }

    /// Write one HASH entry line.
    pub fn write_hash_entry(key: &[u8], value: &[u8], write: &mut impl FnMut(&[u8])) {
        let mut scratch = [0u8; 64];
        let key_tok = format_hash_key(key, &mut scratch);
        write(key_tok);
        write(b" ");
        write_hash_value(value, write);
        write(b"\n");
    }

    fn format_hash_value_len(value: &[u8]) -> usize {
        if value.len() > 8 {
            value.len() * 2
        } else {
            dec_len(value_as_u64(value))
        }
    }

    fn write_hash_value(value: &[u8], write: &mut impl FnMut(&[u8])) {
        if value.len() > 8 {
            const HEX: &[u8] = b"0123456789abcdef";
            let mut chunk = [0u8; 16];
            let mut i = 0usize;
            while i < value.len() {
                let n = (value.len() - i).min(8);
                for (j, b) in value[i..i + n].iter().enumerate() {
                    chunk[j * 2] = HEX[(b >> 4) as usize];
                    chunk[j * 2 + 1] = HEX[(b & 0xf) as usize];
                }
                write(&chunk[..n * 2]);
                i += n;
            }
        } else {
            write_dec(value_as_u64(value), write);
        }
    }

    /// Parse a decimal `u32` wire token.
    pub fn parse_u32_token(tok: &[u8]) -> Option<u32> {
        if tok.is_empty() || tok.len() > 10 {
            return None;
        }
        let mut n = 0u32;
        for &b in tok {
            if !b.is_ascii_digit() {
                return None;
            }
            n = n.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
        }
        Some(n)
    }

    /// Parse a decimal `u64` wire token.
    pub fn parse_u64_token(tok: &[u8]) -> Option<u64> {
        if tok.is_empty() || tok.len() > 20 {
            return None;
        }
        let mut n = 0u64;
        for &b in tok {
            if !b.is_ascii_digit() {
                return None;
            }
            n = n.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
        }
        Some(n)
    }

    /// Parse a dotted IPv4 wire token into network-order bytes.
    pub fn parse_ipv4_token(tok: &[u8]) -> Option<[u8; 4]> {
        let s = core::str::from_utf8(tok).ok()?;
        let mut out = [0u8; 4];
        let mut parts = 0u8;
        let mut acc = 0u32;
        let mut digits = 0u8;
        for b in s.bytes() {
            if b == b'.' {
                if digits == 0 || parts >= 3 {
                    return None;
                }
                out[parts as usize] = acc as u8;
                acc = 0;
                digits = 0;
                parts += 1;
            } else if b.is_ascii_digit() {
                digits += 1;
                if digits > 3 {
                    return None;
                }
                acc = acc * 10 + u32::from(b - b'0');
                if acc > 255 {
                    return None;
                }
            } else {
                return None;
            }
        }
        if parts != 3 || digits == 0 {
            return None;
        }
        out[3] = acc as u8;
        Some(out)
    }
}
