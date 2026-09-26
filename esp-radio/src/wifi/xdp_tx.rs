//! Zero-copy same-iface Tx for feature `xdp`.
//!
//! RX / XDP path calls [`transmit`], which claims an RX-buffer token and
//! immediately hands off via `esp_wifi_internal_tx_by_ref` (no software queue,
//! no batch task, no NO_MEM retry).

#![allow(missing_docs)]

use core::ffi::c_void;

use portable_atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use super::xdp_prof::{self, Site, Span};
use crate::sys::include;

/// Concurrent claim slots; must stay ≥ Wi-Fi dynamic TX/RX pool under burst.
const TX_TOKEN_CAPACITY: usize = 64;
const TX_TOKEN_SLOT_EMPTY: usize = 0;
const TX_TOKEN_SLOT_OWNED: usize = 1;
const TX_TOKEN_SLOT_RELEASING: usize = 2;
/// Print one full flow timestamp line every N successful attempt starts.
const FLOW_SAMPLE_EVERY: u32 = 100;

/// Result of [`transmit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxOutcome {
    Ok,
    FailBadBuf,
    FailToken,
    FailWifi(i32),
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

#[unsafe(link_section = ".bss")]
static STAT_ENQUEUED: AtomicU32 = AtomicU32::new(0);
static STAT_ENQ_FAIL_CLAIM: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_OK: AtomicU32 = AtomicU32::new(0);
static STAT_NOMEM_DROP_IDLE: AtomicU32 = AtomicU32::new(0);
static STAT_NOMEM_DROP_RQ_FAIL: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_FAIL: AtomicU32 = AtomicU32::new(0);
static STAT_LAST_DRAIN_RC: AtomicU32 = AtomicU32::new(0);
static STAT_TX_DONE: AtomicU32 = AtomicU32::new(0);
static STAT_INFLIGHT_PEAK: AtomicU32 = AtomicU32::new(0);
static STAT_LOG_DROPPED: AtomicU32 = AtomicU32::new(0);
static STAT_FLOW_SAMPLES: AtomicU32 = AtomicU32::new(0);
static FLOW_TICK: AtomicU32 = AtomicU32::new(0);
static LAST_HANDOFF_US: AtomicU32 = AtomicU32::new(0);
static TX_DONE_SAMPLE: AtomicU32 = AtomicU32::new(0);

#[unsafe(link_section = ".bss")]
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
    queued: AtomicBool,
    drain_rc: AtomicU32,
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
            queued: AtomicBool::new(false),
            drain_rc: AtomicU32::new(include::ESP_OK),
        }
    }

    fn claim(&self, eb: *mut c_void, ifx: u32, data: *mut u8, len: usize) -> bool {
        if eb.is_null()
            || data.is_null()
            || len == 0
            || self
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
        self.drain_rc.store(include::ESP_OK, Ordering::Release);
        self.queued.store(true, Ordering::Release);
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
        self.queued.store(false, Ordering::Release);
        self.state.store(TX_TOKEN_SLOT_EMPTY, Ordering::Release);
    }
}

#[unsafe(link_section = ".bss")]
static TX_TOKENS: [RxBufferOwnership; TX_TOKEN_CAPACITY] =
    [const { RxBufferOwnership::new() }; TX_TOKEN_CAPACITY];
static TX_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn arm_zero_copy() -> i32 {
    unsafe {
        include::esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free))
    }
}

/// Claim token + immediate `tx_by_ref`. No queue / retry.
pub(crate) fn transmit(ifx: u32, window: &mut [u8], eb: *mut c_void) -> TxOutcome {
    let t0 = now_us();
    let _span = Span::enter(Site::TxEnqueue);
    if eb.is_null() || window.is_empty() {
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    let Some((_idx, token)) = claim_token(eb, ifx, window.as_mut_ptr(), window.len()) else {
        STAT_ENQ_FAIL_CLAIM.fetch_add(1, Ordering::Relaxed);
        push_log(TxEvKind::EnqClaimFail, 0);
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailToken;
    };
    let t_claim = now_us();
    STAT_ENQUEUED.fetch_add(1, Ordering::Relaxed);

    let data = token.data.load(Ordering::Acquire);
    let len = token.len.load(Ordering::Acquire);
    let rc = {
        let _span = Span::enter(Site::TxDrainOne);
        xdp_prof::hit_here();
        if data.is_null() || len == 0 {
            include::ESP_ERR_INVALID_ARG as i32
        } else {
            unsafe {
                include::esp_wifi_internal_tx_by_ref(
                    ifx,
                    data.cast(),
                    len,
                    (token as *const RxBufferOwnership).cast_mut().cast(),
                )
            }
        }
    };
    let t_txref = now_us();
    maybe_tsflow(t0, t_claim, t_txref, rc);

    token.drain_rc.store(rc as u32, Ordering::Release);
    token.queued.store(false, Ordering::Release);
    token.release();

    if rc == include::ESP_ERR_NO_MEM as i32 {
        let infl = TX_IN_FLIGHT.load(Ordering::Acquire);
        STAT_LAST_DRAIN_RC.store(rc as u32, Ordering::Relaxed);
        if infl > 0 {
            STAT_NOMEM_DROP_RQ_FAIL.fetch_add(1, Ordering::Relaxed);
            push_log(TxEvKind::NomemDropRqFail, rc);
        } else {
            STAT_NOMEM_DROP_IDLE.fetch_add(1, Ordering::Relaxed);
            push_log(TxEvKind::NomemDropIdle, rc);
        }
        return TxOutcome::FailWifi(rc);
    }
    if rc != include::ESP_OK as i32 {
        STAT_DRAIN_FAIL.fetch_add(1, Ordering::Relaxed);
        STAT_LAST_DRAIN_RC.store(rc as u32, Ordering::Relaxed);
        push_log(TxEvKind::DrainFail, rc);
        return TxOutcome::FailWifi(rc);
    }

    LAST_HANDOFF_US.store(t_txref, Ordering::Relaxed);
    TX_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
    note_inflight_peak();
    STAT_DRAIN_OK.fetch_add(1, Ordering::Relaxed);
    TxOutcome::Ok
}

/// Wi-Fi finished an air TX. Do not call `tx_by_ref` here.
pub(crate) fn on_tx_done() {
    STAT_TX_DONE.fetch_add(1, Ordering::Relaxed);
    let _ = TX_IN_FLIGHT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| {
        Some(x.saturating_sub(1))
    });
    let n = TX_DONE_SAMPLE.fetch_add(1, Ordering::Relaxed);
    if n % FLOW_SAMPLE_EVERY == 0 {
        let now = now_us();
        let handoff = LAST_HANDOFF_US.load(Ordering::Relaxed);
        let air = now.wrapping_sub(handoff);
        warn!("tsflow_done n={n} us_air={air} infl={}", in_flight());
    }
}

/// Always 0 — no software pending queue.
pub fn pending_len() -> usize {
    0
}

pub fn in_flight() -> usize {
    TX_IN_FLIGHT.load(Ordering::Acquire)
}

/// No-op stub kept for API/compat (batch path removed).
pub fn drain_batch(_max: usize) -> usize {
    0
}

pub fn stats() -> TxStats {
    let claim = STAT_ENQ_FAIL_CLAIM.load(Ordering::Relaxed);
    let drop_idle = STAT_NOMEM_DROP_IDLE.load(Ordering::Relaxed);
    let drop_rq = STAT_NOMEM_DROP_RQ_FAIL.load(Ordering::Relaxed);
    TxStats {
        enqueued: STAT_ENQUEUED.load(Ordering::Relaxed),
        enqueue_fail_token: claim,
        enqueue_fail_claim: claim,
        enqueue_fail_ring: 0,
        drain_ok: STAT_DRAIN_OK.load(Ordering::Relaxed),
        drain_nomem_requeue: 0,
        drain_nomem_drop_idle: drop_idle,
        drain_nomem_drop_rq_fail: drop_rq,
        drain_nomem_drop: drop_idle.saturating_add(drop_rq),
        drain_fail_other: STAT_DRAIN_FAIL.load(Ordering::Relaxed),
        last_drain_rc: STAT_LAST_DRAIN_RC.load(Ordering::Relaxed) as i32,
        drain_skip_stale: 0,
        drain_busy: 0,
        drain_calls: 0,
        kicks: 0,
        tx_done: STAT_TX_DONE.load(Ordering::Relaxed),
        pending: 0,
        pending_peak: 0,
        in_flight: in_flight() as u32,
        in_flight_peak: STAT_INFLIGHT_PEAK.load(Ordering::Relaxed),
        log_dropped: STAT_LOG_DROPPED.load(Ordering::Relaxed),
        flow_samples: STAT_FLOW_SAMPLES.load(Ordering::Relaxed),
    }
}

pub fn snapshot_log(out: &mut [TxLogEntry]) -> usize {
    let n = TX_LOG_N.load(Ordering::Acquire).min(TX_LOG_CAP);
    if n == 0 || out.is_empty() {
        return 0;
    }
    let take = n.min(out.len());
    let w = TX_LOG_W.load(Ordering::Acquire);
    let start = if n < TX_LOG_CAP {
        0
    } else {
        w % TX_LOG_CAP
    };
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
        &STAT_DRAIN_OK,
        &STAT_NOMEM_DROP_IDLE,
        &STAT_NOMEM_DROP_RQ_FAIL,
        &STAT_DRAIN_FAIL,
        &STAT_LAST_DRAIN_RC,
        &STAT_TX_DONE,
        &STAT_INFLIGHT_PEAK,
        &STAT_LOG_DROPPED,
        &STAT_FLOW_SAMPLES,
        &FLOW_TICK,
        &TX_DONE_SAMPLE,
    ] {
        a.store(0, Ordering::Relaxed);
    }
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

fn maybe_tsflow(t0: u32, t_claim: u32, t_txref: u32, rc: i32) {
    let n = FLOW_TICK.fetch_add(1, Ordering::Relaxed);
    if n % FLOW_SAMPLE_EVERY != 0 {
        return;
    }
    STAT_FLOW_SAMPLES.fetch_add(1, Ordering::Relaxed);
    let us_claim = t_claim.wrapping_sub(t0);
    let us_txref = t_txref.wrapping_sub(t_claim);
    let us_total = t_txref.wrapping_sub(t0);
    warn!(
        "tsflow n={n} us_claim={us_claim} us_txref={us_txref} us_total={us_total} rc={rc} infl={}",
        in_flight()
    );
}

fn push_log(kind: TxEvKind, rc: i32) {
    let infl = in_flight().min(u8::MAX as usize) as u8;
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
    TX_LOG_PEND[slot].store(0, Ordering::Relaxed);
    TX_LOG_INFL[slot].store(infl, Ordering::Relaxed);
    TX_LOG_RC[slot].store(rc as u32, Ordering::Relaxed);
}

fn note_inflight_peak() {
    let n = TX_IN_FLIGHT.load(Ordering::Relaxed) as u32;
    let mut peak = STAT_INFLIGHT_PEAK.load(Ordering::Relaxed);
    while n > peak {
        match STAT_INFLIGHT_PEAK.compare_exchange_weak(
            peak,
            n,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(p) => peak = p,
        }
    }
}

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
    if let Some(token) = token_from_raw(token) {
        token.retain_tx();
    } else {
        error!("xdp: Wi-Fi retained an unknown TX token");
    }
}

unsafe extern "C" fn netstack_free(token: *mut c_void) {
    if let Some(token) = token_from_raw(token) {
        token.release();
    } else {
        error!("xdp: Wi-Fi freed an unknown TX token");
    }
}
