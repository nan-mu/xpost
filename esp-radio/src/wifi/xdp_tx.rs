//! Same-iface Tx for feature `xdp` (no copy on our side).
//!
//! - RX / XDP path ([`transmit`]): claim an RX-buffer token and push it onto a
//!   lock-free ring, then signal [`TX_KICK`]. Never calls the Wi-Fi TX API —
//!   re-entering the blob from its own RX callback stalls its TX engine.
//! - Board TX task ([`poll`]): hands ring frames to `esp_wifi_internal_tx_by_ref`
//!   while driver in-flight < [`set_inflight_cap`]. On cap / `NO_MEM` it stops and
//!   waits for a kick (`netstack_free` / `tx_done`); no busy retry.
//! - Ring full at RX → tail drop (RX buffer freed immediately).
//!
//! Pin budget: each queued or in-flight zero-copy frame holds a ~1.7 KB RX
//! buffer. `RING_CAP + inflight_cap` must leave heap headroom for the Wi-Fi
//! driver to keep allocating RX descriptors — otherwise frames never reach
//! [`transmit`] and show up only as host `sent - tx_enq` loss.

#![allow(missing_docs)]

use core::ffi::c_void;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use super::xdp_prof::{self, Site, Span};
use crate::sys::include;

/// Token pool == ring capacity: a claimed token is always either queued or being handed off.
/// Every queued slot pins an RX buffer until drain (or tail-drop free). With the
/// current blob `netstack_ref` never fires, so in-flight handoffs do **not** keep
/// the RX buffer — only the ring does.
///
/// Keep this small: deep pending + inflight turns air jitter into multi-100ms RTT
/// tails and HOL stalls that show up as loss under longer windows.
const TX_TOKEN_CAPACITY: usize = 16;
const RING_CAP: usize = TX_TOKEN_CAPACITY;
const TX_TOKEN_SLOT_EMPTY: usize = 0;
const TX_TOKEN_SLOT_OWNED: usize = 1;
const TX_TOKEN_SLOT_RELEASING: usize = 2;
/// Print one full flow timestamp line every N handoffs (only while `tsflow on`).
const FLOW_SAMPLE_EVERY: u32 = 100;
/// Soft in-flight gate. Non-zero bounds driver queue depth for RTT
/// (`cap / pps` queueing). `0` = gate off (NO_MEM-only; can fill all 128 TX
/// bufs and recreate 100ms+ tails). Keep small; NO_MEM remains the hard stop.
const DEFAULT_INFLIGHT_CAP: usize = 16;

/// Signaled on RX enqueue and on every `tx_done`; the board TX task waits on it.
pub static TX_KICK: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Result of [`transmit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxOutcome {
    Ok,
    FailBadBuf,
    FailToken,
    FailRing,
}

/// Result of [`poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxPoll {
    /// Ring empty.
    Idle,
    /// Frames pending but driver is at cap / returned `NO_MEM`; wait for [`TX_KICK`].
    Blocked,
}

/// Snapshot of TX counters (heapless; copy out over serial).
#[derive(Clone, Copy, Debug, Default)]
pub struct TxStats {
    pub enqueued: u32,
    pub enqueue_fail_token: u32,
    pub enqueue_fail_claim: u32,
    pub enqueue_fail_ring: u32,
    pub drain_ok: u32,
    pub drain_nomem_requeue: u32,
    pub drain_nomem_drop_idle: u32,
    pub drain_nomem_drop_rq_fail: u32,
    pub drain_nomem_drop: u32,
    pub drain_fail_other: u32,
    pub last_drain_rc: i32,
    pub drain_skip_stale: u32,
    /// Polls that stopped because in-flight hit the cap.
    pub drain_busy: u32,
    pub drain_calls: u32,
    pub kicks: u32,
    pub tx_done: u32,
    pub pending: u32,
    pub pending_peak: u32,
    pub in_flight: u32,
    pub in_flight_peak: u32,
    pub log_dropped: u32,
    /// Times a `tsflow` sample line was printed.
    pub flow_samples: u32,
    pub inflight_cap: u32,
    /// `tx_done` with `tx_status == false` (air TX gave up).
    pub tx_done_fail: u32,
    pub block_max_us: u32,
    pub block_gt10ms: u32,
    pub block_gt100ms: u32,
    /// XDP_TX frames whose (post-program) source MAC is a group address.
    pub grp_src: u32,
    /// `netstack_ref` / `netstack_free` callbacks (zero-copy retain path).
    pub netstack_ref: u32,
    pub netstack_free: u32,
    /// Handoffs where the driver did not retain; inflight cleared on `tx_done`.
    pub no_retain: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TxEvKind {
    EnqClaimFail = 1,
    EnqRingFull = 2,
    NomemRq = 3,
    NomemDropIdle = 4,
    NomemDropRqFail = 5,
    DrainFail = 6,
    SkipStale = 7,
    DrainBusy = 8,
}

impl TxEvKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnqClaimFail => "enq_claim",
            Self::EnqRingFull => "enq_ring",
            Self::NomemRq => "nomem_rq",
            Self::NomemDropIdle => "nomem_drop_idle",
            Self::NomemDropRqFail => "nomem_drop_rqfail",
            Self::DrainFail => "drain_fail",
            Self::SkipStale => "skip_stale",
            Self::DrainBusy => "drain_busy",
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::EnqClaimFail),
            2 => Some(Self::EnqRingFull),
            3 => Some(Self::NomemRq),
            4 => Some(Self::NomemDropIdle),
            5 => Some(Self::NomemDropRqFail),
            6 => Some(Self::DrainFail),
            7 => Some(Self::SkipStale),
            8 => Some(Self::DrainBusy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TxLogEntry {
    pub t_us: u32,
    pub kind: u8,
    pub pending: u8,
    pub inflight: u8,
    pub rc: i32,
}

const TX_LOG_CAP: usize = 32;

static STAT_ENQUEUED: AtomicU32 = AtomicU32::new(0);
static STAT_ENQ_FAIL_CLAIM: AtomicU32 = AtomicU32::new(0);
static STAT_ENQ_FAIL_RING: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_OK: AtomicU32 = AtomicU32::new(0);
static STAT_NOMEM_WAIT: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_FAIL: AtomicU32 = AtomicU32::new(0);
static STAT_LAST_DRAIN_RC: AtomicU32 = AtomicU32::new(0);
static STAT_CAP_WAIT: AtomicU32 = AtomicU32::new(0);
static STAT_POLLS: AtomicU32 = AtomicU32::new(0);
static STAT_KICKS: AtomicU32 = AtomicU32::new(0);
static STAT_TX_DONE: AtomicU32 = AtomicU32::new(0);
static STAT_PENDING_PEAK: AtomicU32 = AtomicU32::new(0);
static STAT_INFLIGHT_PEAK: AtomicU32 = AtomicU32::new(0);
static STAT_LOG_DROPPED: AtomicU32 = AtomicU32::new(0);
static STAT_FLOW_SAMPLES: AtomicU32 = AtomicU32::new(0);
static FLOW_TICK: AtomicU32 = AtomicU32::new(0);
static LAST_HANDOFF_US: AtomicU32 = AtomicU32::new(0);
static TX_DONE_SAMPLE: AtomicU32 = AtomicU32::new(0);
static STAT_TX_DONE_FAIL: AtomicU32 = AtomicU32::new(0);
/// Start (µs) of the current cap/NO_MEM block; 0 = not blocked.
static BLOCK_SINCE_US: AtomicU32 = AtomicU32::new(0);
static STAT_BLOCK_MAX_US: AtomicU32 = AtomicU32::new(0);
static STAT_BLOCK_GT10MS: AtomicU32 = AtomicU32::new(0);
static STAT_BLOCK_GT100MS: AtomicU32 = AtomicU32::new(0);
static TSFLOW_ON: AtomicBool = AtomicBool::new(false);
static STAT_GRP_SRC: AtomicU32 = AtomicU32::new(0);
static STAT_NETSTACK_REF: AtomicU32 = AtomicU32::new(0);
static STAT_NETSTACK_FREE: AtomicU32 = AtomicU32::new(0);
static STAT_NO_RETAIN: AtomicU32 = AtomicU32::new(0);
/// Outstanding handoffs that must clear via `tx_done` (driver skipped retain).
static TX_NEED_DONE: AtomicUsize = AtomicUsize::new(0);
static INFLIGHT_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_INFLIGHT_CAP);

static TX_LOG_KIND: [AtomicU8; TX_LOG_CAP] = [const { AtomicU8::new(0) }; TX_LOG_CAP];
static TX_LOG_PEND: [AtomicU8; TX_LOG_CAP] = [const { AtomicU8::new(0) }; TX_LOG_CAP];
static TX_LOG_INFL: [AtomicU8; TX_LOG_CAP] = [const { AtomicU8::new(0) }; TX_LOG_CAP];
static TX_LOG_T_US: [AtomicU32; TX_LOG_CAP] = [const { AtomicU32::new(0) }; TX_LOG_CAP];
static TX_LOG_RC: [AtomicU32; TX_LOG_CAP] = [const { AtomicU32::new(0) }; TX_LOG_CAP];
static TX_LOG_W: AtomicUsize = AtomicUsize::new(0);
static TX_LOG_N: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct RxBufferOwnership {
    state: AtomicUsize,
    refs: AtomicUsize,
    eb: AtomicPtr<c_void>,
    ifx: AtomicU32,
    data: AtomicPtr<u8>,
    len: AtomicUsize,
    t_enq_us: AtomicU32,
}

impl RxBufferOwnership {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(TX_TOKEN_SLOT_EMPTY),
            refs: AtomicUsize::new(0),
            eb: AtomicPtr::new(core::ptr::null_mut()),
            ifx: AtomicU32::new(0),
            data: AtomicPtr::new(core::ptr::null_mut()),
            len: AtomicUsize::new(0),
            t_enq_us: AtomicU32::new(0),
        }
    }

    fn claim(&self, eb: *mut c_void, ifx: u32, data: *mut u8, len: usize) -> bool {
        if self
            .state
            .compare_exchange(
                TX_TOKEN_SLOT_EMPTY,
                TX_TOKEN_SLOT_OWNED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        self.eb.store(eb, Ordering::Release);
        self.ifx.store(ifx, Ordering::Release);
        self.data.store(data, Ordering::Release);
        self.len.store(len, Ordering::Release);
        self.refs.store(1, Ordering::Release);
        true
    }

    fn retain_tx(&self) {
        if self.state.load(Ordering::Acquire) != TX_TOKEN_SLOT_OWNED {
            return;
        }
        let _ = self.refs.fetch_add(1, Ordering::AcqRel);
    }

    fn release(&self) {
        let mut old = self.refs.load(Ordering::Acquire);
        loop {
            if old == 0 {
                return;
            }
            match self
                .refs
                .compare_exchange_weak(old, old - 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(actual) => old = actual,
            }
        }
        if old != 1 {
            return;
        }
        if self.state.load(Ordering::Acquire) != TX_TOKEN_SLOT_OWNED {
            return;
        }
        self.state.store(TX_TOKEN_SLOT_RELEASING, Ordering::Release);
        let eb = self.eb.swap(core::ptr::null_mut(), Ordering::AcqRel);
        if !eb.is_null() {
            unsafe { include::esp_wifi_internal_free_rx_buffer(eb) };
        }
        self.state.store(TX_TOKEN_SLOT_EMPTY, Ordering::Release);
    }
}

static TX_TOKENS: [RxBufferOwnership; TX_TOKEN_CAPACITY] =
    [const { RxBufferOwnership::new() }; TX_TOKEN_CAPACITY];
static TX_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

// SPSC ring of token indices. Producer: Wi-Fi task RX callback (STA and AP
// callbacks share that task). Consumer: board TX task via `poll`.
static RING: [AtomicU8; RING_CAP] = [const { AtomicU8::new(0) }; RING_CAP];
static RING_HEAD: AtomicUsize = AtomicUsize::new(0);
static RING_TAIL: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn arm_zero_copy() -> i32 {
    unsafe {
        include::esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free))
    }
}

/// RX path: claim token + enqueue + kick. Never calls the Wi-Fi TX API.
#[esp_hal::ram]
pub(crate) fn transmit(ifx: u32, window: &mut [u8], eb: *mut c_void) -> TxOutcome {
    let _span = Span::enter(Site::TxEnqueue);
    if eb.is_null() || window.is_empty() {
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    // 802 source addresses must be individual; a STA cannot send with a group SA
    // (e.g. MAC-swapped broadcast/mDNS). The blob retries such frames until they
    // fail, blocking the TX queue behind them (up to ~0.8 s stalls).
    if window.len() < 12 || window[6] & 1 != 0 {
        STAT_GRP_SRC.fetch_add(1, Ordering::Relaxed);
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    let Some((idx, token)) = claim_token(eb, ifx, window.as_mut_ptr(), window.len()) else {
        STAT_ENQ_FAIL_CLAIM.fetch_add(1, Ordering::Relaxed);
        push_log(TxEvKind::EnqClaimFail, 0);
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailToken;
    };
    if TSFLOW_ON.load(Ordering::Relaxed) {
        token.t_enq_us.store(now_us(), Ordering::Relaxed);
    }
    if !ring_push(idx) {
        STAT_ENQ_FAIL_RING.fetch_add(1, Ordering::Relaxed);
        push_log(TxEvKind::EnqRingFull, 0);
        token.release();
        return TxOutcome::FailRing;
    }
    STAT_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    note_pending_peak();
    STAT_KICKS.fetch_add(1, Ordering::Relaxed);
    TX_KICK.signal(());
    TxOutcome::Ok
}

/// TX task: hand pending frames to Wi-Fi until empty or `NO_MEM`.
pub fn poll() -> TxPoll {
    STAT_POLLS.fetch_add(1, Ordering::Relaxed);
    loop {
        let Some(idx) = ring_peek() else {
            return TxPoll::Idle;
        };
        // Soft cap (`txcap N`): off when 0. Prefer NO_MEM — see DEFAULT_INFLIGHT_CAP.
        let soft_cap = INFLIGHT_CAP.load(Ordering::Relaxed);
        if soft_cap > 0 && TX_IN_FLIGHT.load(Ordering::Acquire) >= soft_cap {
            STAT_CAP_WAIT.fetch_add(1, Ordering::Relaxed);
            note_block_start();
            return TxPoll::Blocked;
        }
        let token = &TX_TOKENS[idx as usize];
        let data = token.data.load(Ordering::Acquire);
        let len = token.len.load(Ordering::Acquire);
        let ifx = token.ifx.load(Ordering::Acquire);
        let flow = TSFLOW_ON.load(Ordering::Relaxed);
        let t0 = if flow { now_us() } else { 0 };
        // Reserve before `tx_by_ref` so same-frame `tx_done` cannot race the
        // NEED_DONE bump (diagnostic / soft-cap counter only).
        TX_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        TX_NEED_DONE.fetch_add(1, Ordering::AcqRel);
        note_inflight_peak();
        let rc = {
            let _span = Span::enter(Site::TxDrainOne);
            xdp_prof::hit_here();
            unsafe {
                include::esp_wifi_internal_tx_by_ref(
                    ifx,
                    data.cast(),
                    len,
                    (token as *const RxBufferOwnership).cast_mut().cast(),
                )
            }
        };
        if rc == include::ESP_ERR_NO_MEM as i32 {
            undo_inflight_reservation();
            STAT_NOMEM_WAIT.fetch_add(1, Ordering::Relaxed);
            STAT_LAST_DRAIN_RC.store(rc as u32, Ordering::Relaxed);
            push_log(TxEvKind::NomemRq, rc);
            note_block_start();
            return TxPoll::Blocked;
        }
        note_block_end();
        ring_pop();
        if rc == include::ESP_OK as i32 {
            let retained = token.refs.load(Ordering::Acquire) >= 2;
            STAT_DRAIN_OK.fetch_add(1, Ordering::Relaxed);
            if retained {
                let _ = TX_NEED_DONE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n > 0).then_some(n - 1)
                });
            } else {
                STAT_NO_RETAIN.fetch_add(1, Ordering::Relaxed);
            }
            if flow {
                let t1 = now_us();
                LAST_HANDOFF_US.store(t1, Ordering::Relaxed);
                maybe_tsflow(token.t_enq_us.load(Ordering::Relaxed), t0, t1);
            }
        } else {
            undo_inflight_reservation();
            STAT_DRAIN_FAIL.fetch_add(1, Ordering::Relaxed);
            STAT_LAST_DRAIN_RC.store(rc as u32, Ordering::Relaxed);
            push_log(TxEvKind::DrainFail, rc);
        }
        token.release();
    }
}

fn undo_inflight_reservation() {
    let _ = TX_NEED_DONE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
        (n > 0).then_some(n - 1)
    });
    let _ = TX_IN_FLIGHT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| {
        Some(x.saturating_sub(1))
    });
}

/// Wi-Fi finished an air TX. Do not call `tx_by_ref` here.
///
/// Inflight for retained zero-copy frames is cleared in [`netstack_free`].
/// This callback only clears the no-retain fallback and re-kicks the TX task —
/// never decrement for every global `tx_done` (smoltcp ACKs would steal slots).
#[esp_hal::ram]
pub(crate) fn on_tx_done(ok: bool) {
    STAT_TX_DONE.fetch_add(1, Ordering::Relaxed);
    if !ok {
        STAT_TX_DONE_FAIL.fetch_add(1, Ordering::Relaxed);
    }
    if TX_NEED_DONE
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n > 0).then_some(n - 1)
        })
        .is_ok()
    {
        // No-retain handoff: driver never called `netstack_free`.
        let _ = TX_IN_FLIGHT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| {
            Some(x.saturating_sub(1))
        });
    }
    TX_KICK.signal(());
    if TSFLOW_ON.load(Ordering::Relaxed) {
        let n = TX_DONE_SAMPLE.fetch_add(1, Ordering::Relaxed);
        if n % FLOW_SAMPLE_EVERY == 0 {
            let air = now_us().wrapping_sub(LAST_HANDOFF_US.load(Ordering::Relaxed));
            warn!("tsflow_done n={n} us_air={air} infl={}", in_flight());
        }
    }
}

fn clear_one_inflight() {
    let _ = TX_IN_FLIGHT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| {
        Some(x.saturating_sub(1))
    });
    TX_KICK.signal(());
}

pub fn pending_len() -> usize {
    RING_TAIL
        .load(Ordering::Acquire)
        .wrapping_sub(RING_HEAD.load(Ordering::Acquire))
}

pub fn in_flight() -> usize {
    TX_IN_FLIGHT.load(Ordering::Acquire)
}

pub fn set_inflight_cap(cap: usize) {
    // `0` disables the soft gate (NO_MEM-only). `txcap 0` from serial is valid.
    INFLIGHT_CAP.store(cap, Ordering::Relaxed);
    TX_KICK.signal(());
}

pub fn set_tsflow(on: bool) {
    TSFLOW_ON.store(on, Ordering::Relaxed);
}

pub fn stats() -> TxStats {
    let nomem = STAT_NOMEM_WAIT.load(Ordering::Relaxed);
    let ring = STAT_ENQ_FAIL_RING.load(Ordering::Relaxed);
    let claim = STAT_ENQ_FAIL_CLAIM.load(Ordering::Relaxed);
    TxStats {
        enqueued: STAT_ENQUEUED.load(Ordering::Relaxed),
        enqueue_fail_token: claim.saturating_add(ring),
        enqueue_fail_claim: claim,
        enqueue_fail_ring: ring,
        drain_ok: STAT_DRAIN_OK.load(Ordering::Relaxed),
        drain_nomem_requeue: nomem,
        drain_nomem_drop_idle: 0,
        drain_nomem_drop_rq_fail: 0,
        drain_nomem_drop: 0,
        drain_fail_other: STAT_DRAIN_FAIL.load(Ordering::Relaxed),
        last_drain_rc: STAT_LAST_DRAIN_RC.load(Ordering::Relaxed) as i32,
        drain_skip_stale: 0,
        drain_busy: STAT_CAP_WAIT.load(Ordering::Relaxed),
        drain_calls: STAT_POLLS.load(Ordering::Relaxed),
        kicks: STAT_KICKS.load(Ordering::Relaxed),
        tx_done: STAT_TX_DONE.load(Ordering::Relaxed),
        pending: pending_len() as u32,
        pending_peak: STAT_PENDING_PEAK.load(Ordering::Relaxed),
        in_flight: in_flight() as u32,
        in_flight_peak: STAT_INFLIGHT_PEAK.load(Ordering::Relaxed),
        log_dropped: STAT_LOG_DROPPED.load(Ordering::Relaxed),
        flow_samples: STAT_FLOW_SAMPLES.load(Ordering::Relaxed),
        inflight_cap: INFLIGHT_CAP.load(Ordering::Relaxed) as u32,
        tx_done_fail: STAT_TX_DONE_FAIL.load(Ordering::Relaxed),
        block_max_us: STAT_BLOCK_MAX_US.load(Ordering::Relaxed),
        block_gt10ms: STAT_BLOCK_GT10MS.load(Ordering::Relaxed),
        block_gt100ms: STAT_BLOCK_GT100MS.load(Ordering::Relaxed),
        grp_src: STAT_GRP_SRC.load(Ordering::Relaxed),
        netstack_ref: STAT_NETSTACK_REF.load(Ordering::Relaxed),
        netstack_free: STAT_NETSTACK_FREE.load(Ordering::Relaxed),
        no_retain: STAT_NO_RETAIN.load(Ordering::Relaxed),
    }
}

pub fn snapshot_log(out: &mut [TxLogEntry]) -> usize {
    let n = TX_LOG_N.load(Ordering::Acquire).min(TX_LOG_CAP);
    if n == 0 || out.is_empty() {
        return 0;
    }
    let take = n.min(out.len());
    let w = TX_LOG_W.load(Ordering::Acquire);
    let start = if n < TX_LOG_CAP { 0 } else { w % TX_LOG_CAP };
    for i in 0..take {
        let idx = (start + i) % TX_LOG_CAP;
        out[i] = TxLogEntry {
            t_us: TX_LOG_T_US[idx].load(Ordering::Relaxed),
            kind: TX_LOG_KIND[idx].load(Ordering::Relaxed),
            pending: TX_LOG_PEND[idx].load(Ordering::Relaxed),
            inflight: TX_LOG_INFL[idx].load(Ordering::Relaxed),
            rc: TX_LOG_RC[idx].load(Ordering::Relaxed) as i32,
        };
    }
    take
}

pub fn clear_stats() {
    for a in [
        &STAT_ENQUEUED,
        &STAT_ENQ_FAIL_CLAIM,
        &STAT_ENQ_FAIL_RING,
        &STAT_DRAIN_OK,
        &STAT_NOMEM_WAIT,
        &STAT_DRAIN_FAIL,
        &STAT_LAST_DRAIN_RC,
        &STAT_CAP_WAIT,
        &STAT_POLLS,
        &STAT_KICKS,
        &STAT_TX_DONE,
        &STAT_PENDING_PEAK,
        &STAT_INFLIGHT_PEAK,
        &STAT_LOG_DROPPED,
        &STAT_FLOW_SAMPLES,
        &FLOW_TICK,
        &TX_DONE_SAMPLE,
        &STAT_TX_DONE_FAIL,
        &STAT_BLOCK_MAX_US,
        &STAT_BLOCK_GT10MS,
        &STAT_BLOCK_GT100MS,
        &STAT_GRP_SRC,
        &STAT_NETSTACK_REF,
        &STAT_NETSTACK_FREE,
        &STAT_NO_RETAIN,
    ] {
        a.store(0, Ordering::Relaxed);
    }
    TX_NEED_DONE.store(0, Ordering::Relaxed);
    for i in 0..TX_LOG_CAP {
        TX_LOG_KIND[i].store(0, Ordering::Relaxed);
        TX_LOG_PEND[i].store(0, Ordering::Relaxed);
        TX_LOG_INFL[i].store(0, Ordering::Relaxed);
        TX_LOG_T_US[i].store(0, Ordering::Relaxed);
        TX_LOG_RC[i].store(0, Ordering::Relaxed);
    }
    TX_LOG_W.store(0, Ordering::Relaxed);
    TX_LOG_N.store(0, Ordering::Relaxed);
}

fn now_us() -> u32 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros() as u32
}

fn maybe_tsflow(t_enq: u32, t0: u32, t1: u32) {
    if !TSFLOW_ON.load(Ordering::Relaxed) {
        return;
    }
    let n = FLOW_TICK.fetch_add(1, Ordering::Relaxed);
    if n % FLOW_SAMPLE_EVERY != 0 {
        return;
    }
    STAT_FLOW_SAMPLES.fetch_add(1, Ordering::Relaxed);
    warn!(
        "tsflow n={n} us_queue={} us_txref={} pend={} infl={}",
        t0.wrapping_sub(t_enq),
        t1.wrapping_sub(t0),
        pending_len(),
        in_flight()
    );
}

fn push_log(kind: TxEvKind, rc: i32) {
    let infl = in_flight().min(u8::MAX as usize) as u8;
    let pend = pending_len().min(u8::MAX as usize) as u8;
    let slot = TX_LOG_W.fetch_add(1, Ordering::AcqRel) % TX_LOG_CAP;
    let prev_n = TX_LOG_N.load(Ordering::Relaxed);
    if prev_n >= TX_LOG_CAP {
        STAT_LOG_DROPPED.fetch_add(1, Ordering::Relaxed);
    } else {
        let _ = TX_LOG_N.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            Some((n + 1).min(TX_LOG_CAP))
        });
    }
    TX_LOG_T_US[slot].store(now_us(), Ordering::Relaxed);
    TX_LOG_KIND[slot].store(kind as u8, Ordering::Relaxed);
    TX_LOG_PEND[slot].store(pend, Ordering::Relaxed);
    TX_LOG_INFL[slot].store(infl, Ordering::Relaxed);
    TX_LOG_RC[slot].store(rc as u32, Ordering::Relaxed);
}

fn note_peak(stat: &AtomicU32, n: u32) {
    let mut peak = stat.load(Ordering::Relaxed);
    while n > peak {
        match stat.compare_exchange_weak(peak, n, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(p) => peak = p,
        }
    }
}

fn note_block_start() {
    let _ = BLOCK_SINCE_US.compare_exchange(0, now_us().max(1), Ordering::Relaxed, Ordering::Relaxed);
}

fn note_block_end() {
    let since = BLOCK_SINCE_US.swap(0, Ordering::Relaxed);
    if since == 0 {
        return;
    }
    let dur = now_us().wrapping_sub(since);
    note_peak(&STAT_BLOCK_MAX_US, dur);
    if dur > 10_000 {
        STAT_BLOCK_GT10MS.fetch_add(1, Ordering::Relaxed);
    }
    if dur > 100_000 {
        STAT_BLOCK_GT100MS.fetch_add(1, Ordering::Relaxed);
    }
}

fn note_inflight_peak() {
    note_peak(&STAT_INFLIGHT_PEAK, in_flight() as u32);
}

fn note_pending_peak() {
    note_peak(&STAT_PENDING_PEAK, pending_len() as u32);
}

#[esp_hal::ram]
fn ring_push(idx: u8) -> bool {
    let tail = RING_TAIL.load(Ordering::Relaxed);
    let head = RING_HEAD.load(Ordering::Acquire);
    if tail.wrapping_sub(head) >= RING_CAP {
        return false;
    }
    RING[tail % RING_CAP].store(idx, Ordering::Relaxed);
    RING_TAIL.store(tail.wrapping_add(1), Ordering::Release);
    true
}

fn ring_peek() -> Option<u8> {
    let head = RING_HEAD.load(Ordering::Relaxed);
    let tail = RING_TAIL.load(Ordering::Acquire);
    if head == tail {
        return None;
    }
    Some(RING[head % RING_CAP].load(Ordering::Relaxed))
}

fn ring_pop() {
    let head = RING_HEAD.load(Ordering::Relaxed);
    RING_HEAD.store(head.wrapping_add(1), Ordering::Release);
}

#[esp_hal::ram]
fn claim_token(
    eb: *mut c_void,
    ifx: u32,
    data: *mut u8,
    len: usize,
) -> Option<(u8, &'static RxBufferOwnership)> {
    for (i, token) in TX_TOKENS.iter().enumerate() {
        if token.claim(eb, ifx, data, len) {
            return Some((i as u8, token));
        }
    }
    None
}

fn token_from_raw(token: *mut c_void) -> Option<&'static RxBufferOwnership> {
    if token.is_null() {
        return None;
    }
    let base = TX_TOKENS.as_ptr() as usize;
    let addr = token as usize;
    if addr < base {
        return None;
    }
    let off = addr - base;
    if off % core::mem::size_of::<RxBufferOwnership>() != 0 {
        return None;
    }
    let i = off / core::mem::size_of::<RxBufferOwnership>();
    TX_TOKENS.get(i)
}

unsafe extern "C" fn netstack_ref(token: *mut c_void) {
    STAT_NETSTACK_REF.fetch_add(1, Ordering::Relaxed);
    if let Some(token) = token_from_raw(token) {
        token.retain_tx();
    } else {
        error!("xdp: Wi-Fi retained an unknown TX token");
    }
}

unsafe extern "C" fn netstack_free(token: *mut c_void) {
    STAT_NETSTACK_FREE.fetch_add(1, Ordering::Relaxed);
    if let Some(token) = token_from_raw(token) {
        // XDP-only completion: do not couple to global `tx_done` (smoltcp).
        clear_one_inflight();
        token.release();
    } else {
        error!("xdp: Wi-Fi freed an unknown TX token");
    }
}
