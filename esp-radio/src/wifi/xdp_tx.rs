//! Zero-copy same-iface Tx for feature `xdp`.
//!
//! # Split ownership
//!
//! - **XDP / RX path** ([`transmit`]): claim an RX-buffer token and enqueue only.
//!   Never calls `esp_wifi_internal_tx_by_ref`. Signals [`TX_KICK`].
//! - **Batch entity** ([`drain_batch`], [`on_tx_done`], firmware task): decides
//!   *when* to hand off up to `max` frames per call.
//!
//! Keeping TX out of the RX callback lets the Wi-Fi task reclaim TX slots and
//! amortizes handoff over a queue (batch) instead of one-packet-one-call.

use core::ffi::c_void;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use super::xdp_prof::{self, Site, Span};
use crate::sys::include;

const TX_TOKEN_CAPACITY: usize = 16;
const TX_TOKEN_SLOT_EMPTY: usize = 0;
const TX_TOKEN_SLOT_OWNED: usize = 1;
const TX_TOKEN_SLOT_RELEASING: usize = 2;
const PENDING_EMPTY: u8 = 0;

/// Wakes the firmware TX batch task after enqueue / tx_done.
pub static TX_KICK: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// Result of [`transmit`]: enqueue outcome only (no wifi handoff here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxOutcome {
    Ok,
    FailBadBuf,
    FailToken,
    /// Unused on the enqueue-only path; retained for `Prog` wiring compatibility.
    #[allow(dead_code)]
    FailWifi(i32),
}

/// Snapshot of queue / drain counters (heapless; copy out over serial).
#[derive(Clone, Copy, Debug, Default)]
pub struct TxStats {
    /// Frames accepted into the pending queue.
    pub enqueued: u32,
    /// Token pool or ring full at enqueue.
    pub enqueue_fail_token: u32,
    /// `tx_by_ref` returned OK.
    pub drain_ok: u32,
    /// `NO_MEM` while in-flight; frame put back on the queue.
    pub drain_nomem_requeue: u32,
    /// `NO_MEM` with nothing in-flight; frame dropped.
    pub drain_nomem_drop: u32,
    /// Other non-OK `tx_by_ref` results.
    pub drain_fail_other: u32,
    /// Calls to [`drain_batch`].
    pub drain_calls: u32,
    /// Times [`TX_KICK`] was signaled.
    pub kicks: u32,
    /// Current pending queue depth.
    pub pending: u32,
    /// High-water pending depth.
    pub pending_peak: u32,
    /// Successful handoffs awaiting `netstack_free`.
    pub in_flight: u32,
    /// High-water in-flight count.
    pub in_flight_peak: u32,
}

#[unsafe(link_section = ".bss")]
static STAT_ENQUEUED: AtomicU32 = AtomicU32::new(0);
static STAT_ENQ_FAIL_TOKEN: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_OK: AtomicU32 = AtomicU32::new(0);
static STAT_NOMEM_REQUEUE: AtomicU32 = AtomicU32::new(0);
static STAT_NOMEM_DROP: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_FAIL: AtomicU32 = AtomicU32::new(0);
static STAT_DRAIN_CALLS: AtomicU32 = AtomicU32::new(0);
static STAT_KICKS: AtomicU32 = AtomicU32::new(0);
static STAT_PENDING_PEAK: AtomicU32 = AtomicU32::new(0);
static STAT_INFLIGHT_PEAK: AtomicU32 = AtomicU32::new(0);

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

#[unsafe(link_section = ".bss")]
static PENDING: [AtomicU8; TX_TOKEN_CAPACITY] =
    [const { AtomicU8::new(PENDING_EMPTY) }; TX_TOKEN_CAPACITY];
static PENDING_HEAD: AtomicUsize = AtomicUsize::new(0);
static PENDING_LEN: AtomicUsize = AtomicUsize::new(0);
static DRAINING: AtomicBool = AtomicBool::new(false);
static TX_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn arm_zero_copy() -> i32 {
    unsafe {
        include::esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free))
    }
}

/// Enqueue only. XDP / RX must not call `tx_by_ref`.
pub(crate) fn transmit(ifx: u32, window: &mut [u8], eb: *mut c_void) -> TxOutcome {
    let _span = Span::enter(Site::TxEnqueue);
    if eb.is_null() || window.is_empty() {
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    let Some((idx, token)) = claim_token(eb, ifx, window.as_mut_ptr(), window.len()) else {
        STAT_ENQ_FAIL_TOKEN.fetch_add(1, Ordering::Relaxed);
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailToken;
    };
    if !enqueue(idx) {
        token.queued.store(false, Ordering::Release);
        token.release();
        STAT_ENQ_FAIL_TOKEN.fetch_add(1, Ordering::Relaxed);
        return TxOutcome::FailToken;
    }
    STAT_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    note_pending_peak();
    kick();
    TxOutcome::Ok
}

/// Drain up to `max` queued frames via zero-copy `tx_by_ref`.
///
/// Called by the batch task and [`on_tx_done`]. Returns how many handoffs
/// succeeded (`ESP_OK`).
pub fn drain_batch(max: usize) -> usize {
    let _span = Span::enter(Site::TxDrainBatch);
    STAT_DRAIN_CALLS.fetch_add(1, Ordering::Relaxed);
    try_drain(max)
}

/// Wi-Fi finished an air TX — continue the queue.
pub(crate) fn on_tx_done() {
    let n = drain_batch(4);
    if n > 0 || pending_len() > 0 {
        kick();
    }
}

/// Current pending queue depth.
pub fn pending_len() -> usize {
    PENDING_LEN.load(Ordering::Acquire)
}

/// Successful handoffs not yet freed by Wi-Fi.
pub fn in_flight() -> usize {
    TX_IN_FLIGHT.load(Ordering::Acquire)
}

/// Copy counters for serial / harness.
pub fn stats() -> TxStats {
    TxStats {
        enqueued: STAT_ENQUEUED.load(Ordering::Relaxed),
        enqueue_fail_token: STAT_ENQ_FAIL_TOKEN.load(Ordering::Relaxed),
        drain_ok: STAT_DRAIN_OK.load(Ordering::Relaxed),
        drain_nomem_requeue: STAT_NOMEM_REQUEUE.load(Ordering::Relaxed),
        drain_nomem_drop: STAT_NOMEM_DROP.load(Ordering::Relaxed),
        drain_fail_other: STAT_DRAIN_FAIL.load(Ordering::Relaxed),
        drain_calls: STAT_DRAIN_CALLS.load(Ordering::Relaxed),
        kicks: STAT_KICKS.load(Ordering::Relaxed),
        pending: pending_len() as u32,
        pending_peak: STAT_PENDING_PEAK.load(Ordering::Relaxed),
        in_flight: in_flight() as u32,
        in_flight_peak: STAT_INFLIGHT_PEAK.load(Ordering::Relaxed),
    }
}

/// Reset drain / enqueue counters (does not touch in-flight frames).
pub fn clear_stats() {
    for a in [
        &STAT_ENQUEUED,
        &STAT_ENQ_FAIL_TOKEN,
        &STAT_DRAIN_OK,
        &STAT_NOMEM_REQUEUE,
        &STAT_NOMEM_DROP,
        &STAT_DRAIN_FAIL,
        &STAT_DRAIN_CALLS,
        &STAT_KICKS,
        &STAT_PENDING_PEAK,
        &STAT_INFLIGHT_PEAK,
    ] {
        a.store(0, Ordering::Relaxed);
    }
}

fn kick() {
    STAT_KICKS.fetch_add(1, Ordering::Relaxed);
    TX_KICK.signal(());
}

fn note_pending_peak() {
    let n = PENDING_LEN.load(Ordering::Relaxed) as u32;
    let mut peak = STAT_PENDING_PEAK.load(Ordering::Relaxed);
    while n > peak {
        match STAT_PENDING_PEAK.compare_exchange_weak(
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

fn enqueue(idx: u8) -> bool {
    loop {
        let len = PENDING_LEN.load(Ordering::Acquire);
        if len >= TX_TOKEN_CAPACITY {
            return false;
        }
        if PENDING_LEN
            .compare_exchange(len, len + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        let head = PENDING_HEAD.load(Ordering::Acquire);
        let slot = (head + len) % TX_TOKEN_CAPACITY;
        PENDING[slot].store(idx + 1, Ordering::Release);
        return true;
    }
}

fn enqueue_front(idx: u8) -> bool {
    loop {
        let len = PENDING_LEN.load(Ordering::Acquire);
        if len >= TX_TOKEN_CAPACITY {
            return false;
        }
        if PENDING_LEN
            .compare_exchange(len, len + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        let head = PENDING_HEAD.load(Ordering::Acquire);
        let new_head = (head + TX_TOKEN_CAPACITY - 1) % TX_TOKEN_CAPACITY;
        if PENDING_HEAD
            .compare_exchange(head, new_head, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            PENDING_LEN.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        PENDING[new_head].store(idx + 1, Ordering::Release);
        return true;
    }
}

fn dequeue() -> Option<u8> {
    loop {
        let len = PENDING_LEN.load(Ordering::Acquire);
        if len == 0 {
            return None;
        }
        let head = PENDING_HEAD.load(Ordering::Acquire);
        let slot = PENDING[head].load(Ordering::Acquire);
        if slot == PENDING_EMPTY {
            continue;
        }
        let idx = slot - 1;
        if PENDING_HEAD
            .compare_exchange(
                head,
                (head + 1) % TX_TOKEN_CAPACITY,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            continue;
        }
        PENDING[head].store(PENDING_EMPTY, Ordering::Release);
        PENDING_LEN.fetch_sub(1, Ordering::AcqRel);
        return Some(idx);
    }
}

fn try_drain(budget: usize) -> usize {
    if budget == 0 || DRAINING.swap(true, Ordering::AcqRel) {
        return 0;
    }
    let mut ok = 0usize;
    for _ in 0..budget {
        let Some(idx) = dequeue() else {
            break;
        };
        let token = &TX_TOKENS[idx as usize];
        if !token.queued.load(Ordering::Acquire)
            || token.state.load(Ordering::Acquire) != TX_TOKEN_SLOT_OWNED
        {
            continue;
        }
        let ifx = token.ifx.load(Ordering::Acquire);
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
        if rc == include::ESP_ERR_NO_MEM as i32 {
            if TX_IN_FLIGHT.load(Ordering::Acquire) > 0 && enqueue_front(idx) {
                STAT_NOMEM_REQUEUE.fetch_add(1, Ordering::Relaxed);
                break;
            }
            token.drain_rc.store(rc as u32, Ordering::Release);
            token.queued.store(false, Ordering::Release);
            token.release();
            super::xdp::current().count_failed_tx_wifi(rc);
            STAT_NOMEM_DROP.fetch_add(1, Ordering::Relaxed);
            break;
        }
        token.drain_rc.store(rc as u32, Ordering::Release);
        token.queued.store(false, Ordering::Release);
        token.release();
        if rc != include::ESP_OK as i32 {
            super::xdp::current().count_failed_tx_wifi(rc);
            STAT_DRAIN_FAIL.fetch_add(1, Ordering::Relaxed);
        } else {
            TX_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
            note_inflight_peak();
            STAT_DRAIN_OK.fetch_add(1, Ordering::Relaxed);
            ok += 1;
        }
    }
    DRAINING.store(false, Ordering::Release);
    ok
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
        let _ = TX_IN_FLIGHT.fetch_update(Ordering::AcqRel, Ordering::Acquire, |x| {
            Some(x.saturating_sub(1))
        });
        kick();
    } else {
        error!("xdp: Wi-Fi released an unknown TX token");
    }
}

fn token_from_raw(ptr: *mut c_void) -> Option<&'static RxBufferOwnership> {
    let ptr = ptr.cast::<RxBufferOwnership>();
    TX_TOKENS
        .iter()
        .find(|token| core::ptr::eq(*token as *const RxBufferOwnership, ptr.cast_const()))
}
