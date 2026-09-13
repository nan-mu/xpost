//! Zero-copy XDP_TX Wi-Fi egress.
//!
//! `esp_wifi_internal_tx_by_ref` asynchronously reads the supplied packet
//! pointer. RX owns that packet through `eb`, so each in-flight transmission
//! uses a fixed BSS token that delays `eb` reclamation until both the RX
//! callback and Wi-Fi's netstack-free callback release their references.

use core::ffi::c_void;

use portable_atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

use super::PacketWindow;
use crate::counters::egress::{EGRESS, EgressStats};
use crate::xdp::state::{
    ESP_OK, TX_TOKEN_CAPACITY, TX_TOKEN_SLOT_EMPTY, TX_TOKEN_SLOT_OWNED, TX_TOKEN_SLOT_RELEASING,
};

unsafe extern "C" {
    fn esp_wifi_internal_tx_by_ref(
        ifx: u32,
        buffer: *mut c_void,
        len: usize,
        netstack_buf: *mut c_void,
    ) -> i32;
    fn esp_wifi_internal_reg_netstack_buf_cb(
        ref_: Option<unsafe extern "C" fn(*mut c_void)>,
        free: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> i32;
    fn esp_wifi_internal_free_rx_buffer(buffer: *mut c_void);
}

/// A stable handle passed as `netstack_buf`; it does not contain packet data.
#[repr(C)]
struct RxBufferOwnership {
    state: AtomicUsize,
    refs: AtomicUsize,
    eb: AtomicPtr<c_void>,
}

impl RxBufferOwnership {
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(TX_TOKEN_SLOT_EMPTY),
            refs: AtomicUsize::new(0),
            eb: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    /// Claim the token with the RX callback's initial ownership reference.
    fn claim(&self, eb: *mut c_void) -> bool {
        if eb.is_null()
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
        self.refs.store(1, Ordering::Release);
        true
    }

    /// Retain the asynchronous Wi-Fi TX reference.
    fn retain_tx(&self) {
        if self.state.load(Ordering::Acquire) != TX_TOKEN_SLOT_OWNED {
            EgressStats::bump(&EGRESS.retain_inactive);
            log::error!("[xdp][egress] retain on inactive TX token");
            return;
        }
        let old = self.refs.fetch_add(1, Ordering::AcqRel);
        if old == 0 {
            EgressStats::bump(&EGRESS.retain_unref);
            log::error!("[xdp][egress] retain on unreferenced TX token");
        }
    }

    /// Release either the RX callback's reference or Wi-Fi's TX reference.
    fn release(&self) {
        let mut old = self.refs.load(Ordering::Acquire);
        loop {
            if old == 0 {
                EgressStats::bump(&EGRESS.release_dup);
                log::error!("[xdp][egress] duplicate TX token release");
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
            EgressStats::bump(&EGRESS.release_inactive);
            log::error!("[xdp][egress] final release on inactive TX token");
            return;
        }
        self.state.store(TX_TOKEN_SLOT_RELEASING, Ordering::Release);
        let eb = self.eb.swap(core::ptr::null_mut(), Ordering::AcqRel);
        if !eb.is_null() {
            // This is deliberately outside an XDP invoke critical section.
            unsafe { esp_wifi_internal_free_rx_buffer(eb) };
        }
        self.state.store(TX_TOKEN_SLOT_EMPTY, Ordering::Release);
    }
}

static TX_CALLBACKS_READY: AtomicBool = AtomicBool::new(false);
#[unsafe(link_section = ".bss")]
static TX_TOKENS: [RxBufferOwnership; TX_TOKEN_CAPACITY] =
    [const { RxBufferOwnership::new() }; TX_TOKEN_CAPACITY];

/// Register callbacks used by `esp_wifi_internal_tx_by_ref`.
///
/// Call only after `esp_radio::wifi::new` has initialized the Wi-Fi driver.
pub(super) fn arm_zero_copy() {
    let rc =
        unsafe { esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free)) };
    TX_CALLBACKS_READY.store(rc == ESP_OK, Ordering::Release);
    if rc != ESP_OK {
        EgressStats::bump(&EGRESS.arm_callbacks);
        log::warn!("[xdp][egress] netstack callbacks unavailable rc={rc}; XDP_TX drops");
    }
}

/// Send `window` without copying the packet. This consumes the RX callback's
/// ownership of `eb` on every path.
#[inline]
pub(super) fn transmit(ifx: u32, frame: &mut [u8], window: PacketWindow, eb: *mut c_void) -> bool {
    if eb.is_null() {
        EgressStats::bump(&EGRESS.null_eb);
        return false;
    }
    let Some(packet) = clip_to_window(frame, window) else {
        free_rx(eb);
        return false;
    };
    if !TX_CALLBACKS_READY.load(Ordering::Acquire) {
        EgressStats::bump(&EGRESS.callbacks_not_ready);
        free_rx(eb);
        return false;
    }
    let Some(token) = claim_token(eb) else {
        EgressStats::bump(&EGRESS.token_exhausted);
        free_rx(eb);
        return false;
    };

    // ESP Wi-Fi invokes `netstack_ref(token)` before it hands the buffer to
    // the asynchronous TX path, then balances it with `netstack_free(token)`.
    let rc = unsafe {
        esp_wifi_internal_tx_by_ref(
            ifx,
            packet.as_mut_ptr().cast(),
            packet.len(),
            (token as *const RxBufferOwnership).cast_mut().cast(),
        )
    };
    token.release(); // RX callback's reference; TX holds its own reference.
    if rc == ESP_OK {
        EgressStats::bump(&EGRESS.tx_ok);
        true
    } else {
        EgressStats::bump(&EGRESS.tx_by_ref);
        false
    }
}

fn claim_token(eb: *mut c_void) -> Option<&'static RxBufferOwnership> {
    TX_TOKENS.iter().find(|token| token.claim(eb))
}

fn clip_to_window<'a>(frame: &'a mut [u8], window: PacketWindow) -> Option<&'a mut [u8]> {
    if window.len == 0 {
        EgressStats::bump(&EGRESS.clip_empty);
        return None;
    }
    let start = frame.as_mut_ptr() as usize;
    let Some(offset) = (window.data as usize).checked_sub(start) else {
        EgressStats::bump(&EGRESS.clip_before_frame);
        return None;
    };
    let Some(end) = offset.checked_add(window.len) else {
        EgressStats::bump(&EGRESS.clip_oob);
        return None;
    };
    if end > frame.len() {
        EgressStats::bump(&EGRESS.clip_oob);
        return None;
    }
    Some(&mut frame[offset..end])
}

fn free_rx(eb: *mut c_void) {
    if !eb.is_null() {
        unsafe { esp_wifi_internal_free_rx_buffer(eb) };
    }
}

unsafe extern "C" fn netstack_ref(token: *mut c_void) {
    if let Some(token) = token_from_raw(token) {
        token.retain_tx();
    } else {
        EgressStats::bump(&EGRESS.ref_unknown_token);
        log::error!("[xdp][egress] Wi-Fi retained an unknown TX token");
    }
}

unsafe extern "C" fn netstack_free(token: *mut c_void) {
    if let Some(token) = token_from_raw(token) {
        token.release();
    } else {
        EgressStats::bump(&EGRESS.free_unknown_token);
        log::error!("[xdp][egress] Wi-Fi released an unknown TX token");
    }
}

fn token_from_raw(ptr: *mut c_void) -> Option<&'static RxBufferOwnership> {
    let ptr = ptr.cast::<RxBufferOwnership>();
    TX_TOKENS
        .iter()
        .find(|token| core::ptr::eq(*token as *const RxBufferOwnership, ptr.cast_const()))
}
