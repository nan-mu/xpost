//! Zero-copy same-iface Tx for feature `xdp`.
//!
//! Ownership protocol: each in-flight transmission holds a BSS token that
//! delays `eb` reclamation until both the RX callback and Wi-Fi's
//! `netstack_free` release their references.
//!
//! Scheduling: [`transmit`] enqueues the frame and may run **at most one**
//! `esp_wifi_internal_tx_by_ref` in the RX callback. Further queued frames are
//! drained from [`on_tx_done`] / `netstack_free` so the Wi-Fi task can reclaim
//! TX slots instead of being starved by a sync TX storm on the RX path.

use core::ffi::c_void;

use portable_atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU8, AtomicUsize, Ordering};

use crate::sys::include;

const TX_TOKEN_CAPACITY: usize = 16;
const TX_TOKEN_SLOT_EMPTY: usize = 0;
const TX_TOKEN_SLOT_OWNED: usize = 1;
const TX_TOKEN_SLOT_RELEASING: usize = 2;
const PENDING_EMPTY: u8 = 0; // slot unused; occupied slots store token_index + 1

/// Result of [`transmit`]: success or which failure arm freed/`will free` `eb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxOutcome {
    Ok,
    /// `eb` was null or `window` empty (buffer freed by caller path).
    FailBadBuf,
    /// All [`TX_TOKEN_CAPACITY`] ownership slots busy.
    FailToken,
    /// `esp_wifi_internal_tx_by_ref` returned non-OK (`rc` preserved).
    ///
    /// Reserved for sync handoff failures. Queued-path failures are counted
    /// inside the drain via [`super::xdp::Prog::count_failed_tx_wifi`].
    FailWifi(i32),
}

/// Stable handle passed as `netstack_buf`; does not contain packet data.
#[repr(C)]
struct RxBufferOwnership {
    state: AtomicUsize,
    refs: AtomicUsize,
    eb: AtomicPtr<c_void>,
    ifx: AtomicU32,
    data: AtomicPtr<u8>,
    len: AtomicUsize,
    /// Still waiting for `tx_by_ref` (queued or mid-drain).
    queued: AtomicBool,
    /// Last `tx_by_ref` rc after a drain attempt on this token.
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
            trace!("xdp: retain on inactive token");
            return;
        }
        let old = self.refs.fetch_add(1, Ordering::AcqRel);
        if old == 0 {
            trace!("xdp: retain on unreferenced token");
        }
    }

    fn release(&self) {
        let mut old = self.refs.load(Ordering::Acquire);
        loop {
            if old == 0 {
                trace!("xdp: duplicate token release");
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
            trace!("xdp: final release on inactive token");
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

/// Register netstack ref/free callbacks used by `esp_wifi_internal_tx_by_ref`.
///
/// Returns the blob status code; caller should surface it via `esp_wifi_result!`.
pub(crate) fn arm_zero_copy() -> i32 {
    unsafe {
        include::esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free))
    }
}

/// Transmit `window` without copying. Consumes RX ownership of `eb` on every path.
pub(crate) fn transmit(ifx: u32, window: &mut [u8], eb: *mut c_void) -> TxOutcome {
    if eb.is_null() || window.is_empty() {
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailBadBuf;
    }
    let Some((idx, token)) = claim_token(eb, ifx, window.as_mut_ptr(), window.len()) else {
        super::xdp::free_rx_buffer(eb);
        return TxOutcome::FailToken;
    };
    if !enqueue(idx) {
        token.queued.store(false, Ordering::Release);
        token.release();
        return TxOutcome::FailToken;
    }
    // At most one handoff in the RX callback; more work waits for tx_done.
    // Drain failures (including this frame) are counted via Prog inside try_drain.
    try_drain(1);
    TxOutcome::Ok
}

/// Wi-Fi finished an air TX — drain more queued zero-copy frames.
pub(crate) fn on_tx_done() {
    try_drain(4);
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

fn try_drain(budget: usize) {
    if budget == 0 || DRAINING.swap(true, Ordering::AcqRel) {
        return;
    }
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
        let rc = if data.is_null() || len == 0 {
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
        };
        token.drain_rc.store(rc as u32, Ordering::Release);
        token.queued.store(false, Ordering::Release);
        // Drop the queue/RX reference; TX holds its own via netstack_ref on OK.
        token.release();
        if rc != include::ESP_OK as i32 {
            super::xdp::current().count_failed_tx_wifi(rc);
            // Give the Wi-Fi task CPU to reclaim TX slots before more handoffs.
            if rc == include::ESP_ERR_NO_MEM as i32 {
                break;
            }
        }
    }
    DRAINING.store(false, Ordering::Release);
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
        // Do not drain here: `tx_by_ref` from inside netstack_free can re-enter
        // the Wi-Fi blob. `on_tx_done` / the next RX `transmit` continue the queue.
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
