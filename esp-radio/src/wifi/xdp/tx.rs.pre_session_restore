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
//!
//! Observation: under `xdp-observe`, call [`super::current`] EspRadioTrace
//! methods directly. No local stats / event-log storage.

#![allow(missing_docs)]

use core::ffi::c_void;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use portable_atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use super::prof::{Site, Span};

use crate::sys::include;

/// Token pool == ring capacity: a claimed token is always either queued or being handed off.
/// Every queued slot pins an RX buffer until drain (or tail-drop free). With the
/// current blob `netstack_ref` never fires, so in-flight handoffs do **not** keep
/// the RX buffer — only the ring does.
///
/// Keep modest: each slot pins an RX buffer until drain. 32 ≈ 40 ms at 800 pps
/// so brief air stalls refill the ring less often than with 16 (fewer `enq_claim`).
const TX_TOKEN_CAPACITY: usize = 32;
const RING_CAP: usize = TX_TOKEN_CAPACITY;
const TX_TOKEN_SLOT_EMPTY: usize = 0;
const TX_TOKEN_SLOT_OWNED: usize = 1;
const TX_TOKEN_SLOT_RELEASING: usize = 2;
/// Print one full flow timestamp line every N handoffs (only while `tsflow on`).
const FLOW_SAMPLE_EVERY: u32 = 100;
/// Soft in-flight gate. Bounds driver queue depth for RTT (`cap/pps` queueing).
/// `0` disables the gate (can fill all 128 TX bufs → 100ms+ tails).
///
/// Formal counters: cap=32 + 15ms whole-ring flush made 400/800 loss ≈ `tx_flush`
/// after brief air stalls, and capped ≥2400 at ~1.3k pps (`enq_claim` dominated).
/// Default 64 keeps a RTT bound (~40 ms at 1600 pps) without that flush tax;
/// NO_MEM remains the hard stop.
const DEFAULT_INFLIGHT_CAP: usize = 64;

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

static FLOW_TICK: AtomicU32 = AtomicU32::new(0);
static LAST_HANDOFF_US: AtomicU32 = AtomicU32::new(0);
static TX_DONE_SAMPLE: AtomicU32 = AtomicU32::new(0);
static TSFLOW_ON: AtomicBool = AtomicBool::new(false);
/// Outstanding handoffs that must clear via `tx_done` (driver skipped retain).
static TX_NEED_DONE: AtomicUsize = AtomicUsize::new(0);
static INFLIGHT_CAP: AtomicUsize = AtomicUsize::new(DEFAULT_INFLIGHT_CAP);

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
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_inflight_cap(INFLIGHT_CAP.load(Ordering::Relaxed));
    }
    unsafe {
        include::esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free))
    }
}

/// RX path: claim token + enqueue + kick. Never calls the Wi-Fi TX API.
#[esp_hal::ram]
pub(crate) fn transmit(ifx: u32, window: &mut [u8], eb: *mut c_void) -> TxOutcome {
    let _span = Span::enter(Site::TxEnqueue);
    if eb.is_null() || window.is_empty() {
        super::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    // 802 source addresses must be individual; a STA cannot send with a group SA
    // (e.g. MAC-swapped broadcast/mDNS). The blob retries such frames until they
    // fail, blocking the TX queue behind them (up to ~0.8 s stalls).
    if window.len() < 12 || window[6] & 1 != 0 {
        #[cfg(feature = "xdp-observe")]
        {
            super::current().tx_grp_src();
        }
        super::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    let Some((idx, token)) = claim_token(eb, ifx, window.as_mut_ptr(), window.len()) else {
        #[cfg(feature = "xdp-observe")]
        {
            super::current().tx_enq_fail_claim();
        }
        super::free_rx_buffer(eb);
        return TxOutcome::FailToken;
    };
    if TSFLOW_ON.load(Ordering::Relaxed) {
        token.t_enq_us.store(now_us(), Ordering::Relaxed);
    }
    if !ring_push(idx) {
        #[cfg(feature = "xdp-observe")]
        {
            super::current().tx_enq_fail_ring();
        }
        token.release();
        return TxOutcome::FailRing;
    }
    #[cfg(feature = "xdp-observe")]
    {
        let prog = super::current();
        prog.tx_enq();
        prog.tx_depth(pending_len(), in_flight());
        prog.tx_kick();
    }
    TX_KICK.signal(());
    TxOutcome::Ok
}

/// TX task: hand pending frames to Wi-Fi until empty or `NO_MEM`.
pub fn poll() -> TxPoll {
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_poll();
    }
    loop {
        let Some(idx) = ring_peek() else {
            return TxPoll::Idle;
        };
        // Soft cap (`txcap N`): off when 0. Prefer a modest default — see DEFAULT_INFLIGHT_CAP.
        let soft_cap = INFLIGHT_CAP.load(Ordering::Relaxed);
        if soft_cap > 0 && TX_IN_FLIGHT.load(Ordering::Acquire) >= soft_cap {
            #[cfg(feature = "xdp-observe")]
            {
                super::current().tx_cap_wait();
            }
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
        let _submit = Span::enter(Site::TxSubmit);
        #[cfg(feature = "xdp-observe")]
        {
            let prog = super::current();
            prog.tx_depth(pending_len(), in_flight());
            prog.tx_submission_begin();
        }
        let rc = {
            let _span = Span::enter(Site::TxDrainOne);
            super::prof::hit_here();
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
            #[cfg(feature = "xdp-observe")]
            {
                super::current().tx_nomem_wait(rc);
            }
            return TxPoll::Blocked;
        }
        #[cfg(feature = "xdp-observe")]
        {
            super::current().tx_block();
        }
        ring_pop();
        if rc == include::ESP_OK as i32 {
            let retained = token.refs.load(Ordering::Acquire) >= 2;
            #[cfg(feature = "xdp-observe")]
            {
                super::current().tx_submitted();
            }
            if retained {
                let _ = TX_NEED_DONE.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n > 0).then_some(n - 1)
                });
            } else {
                #[cfg(feature = "xdp-observe")]
                {
                    super::current().tx_no_retain();
                }
            }
            if flow {
                let t1 = now_us();
                LAST_HANDOFF_US.store(t1, Ordering::Relaxed);
                maybe_tsflow(token.t_enq_us.load(Ordering::Relaxed), t0, t1);
            }
        } else {
            undo_inflight_reservation();
            #[cfg(feature = "xdp-observe")]
            {
                super::current().tx_failed(rc);
            }
        }
        token.release();
        // `_submit` drops here — one attempt end.
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
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_completed(ok);
    }
    #[cfg(not(feature = "xdp-observe"))]
    let _ = ok;
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
        if n.is_multiple_of(FLOW_SAMPLE_EVERY) {
            let _air = now_us().wrapping_sub(LAST_HANDOFF_US.load(Ordering::Relaxed));
            warn!("tsflow_done n={n} us_air={_air} infl={}", in_flight());
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
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_inflight_cap(cap);
    }
    TX_KICK.signal(());
}

pub fn set_tsflow(on: bool) {
    TSFLOW_ON.store(on, Ordering::Relaxed);
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
    if !n.is_multiple_of(FLOW_SAMPLE_EVERY) {
        return;
    }
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_flow_sample();
    }
    warn!(
        "tsflow n={n} us_queue={} us_txref={} pend={} infl={}",
        t0.wrapping_sub(t_enq),
        t1.wrapping_sub(t0),
        pending_len(),
        in_flight()
    );
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
    if !off.is_multiple_of(core::mem::size_of::<RxBufferOwnership>()) {
        return None;
    }
    let i = off / core::mem::size_of::<RxBufferOwnership>();
    TX_TOKENS.get(i)
}

unsafe extern "C" fn netstack_ref(token: *mut c_void) {
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_ns_ref();
    }
    if let Some(token) = token_from_raw(token) {
        token.retain_tx();
    } else {
        error!("xdp: Wi-Fi retained an unknown TX token");
    }
}

unsafe extern "C" fn netstack_free(token: *mut c_void) {
    #[cfg(feature = "xdp-observe")]
    {
        super::current().tx_ns_free();
    }
    if let Some(token) = token_from_raw(token) {
        // XDP-only completion: do not couple to global `tx_done` (smoltcp).
        clear_one_inflight();
        token.release();
    } else {
        error!("xdp: Wi-Fi freed an unknown TX token");
    }
}
