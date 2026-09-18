//! Zero-copy same-iface Tx for feature `xdp`.
//!
//! Ownership protocol: each in-flight transmission holds a BSS token that
//! delays `eb` reclamation until both the RX callback and Wi-Fi's
//! `netstack_free` release their references.

use core::ffi::c_void;

use portable_atomic::{AtomicPtr, AtomicUsize, Ordering};

use crate::sys::include;

const TX_TOKEN_CAPACITY: usize = 16;
const TX_TOKEN_SLOT_EMPTY: usize = 0;
const TX_TOKEN_SLOT_OWNED: usize = 1;
const TX_TOKEN_SLOT_RELEASING: usize = 2;

/// Stable handle passed as `netstack_buf`; does not contain packet data.
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
        self.state.store(TX_TOKEN_SLOT_EMPTY, Ordering::Release);
    }
}

#[unsafe(link_section = ".bss")]
static TX_TOKENS: [RxBufferOwnership; TX_TOKEN_CAPACITY] =
    [const { RxBufferOwnership::new() }; TX_TOKEN_CAPACITY];

/// Register netstack ref/free callbacks used by `esp_wifi_internal_tx_by_ref`.
///
/// Returns the blob status code; caller should surface it via `esp_wifi_result!`.
pub(crate) fn arm_zero_copy() -> i32 {
    unsafe {
        include::esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free))
    }
}

/// Transmit `window` without copying. Consumes RX ownership of `eb` on every path.
///
/// Returns `true` on successful handoff (`esp_wifi_internal_tx_by_ref` == OK).
/// On `false`, `eb` has already been freed (or was null).
pub(crate) fn transmit(ifx: u32, window: &mut [u8], eb: *mut c_void) -> bool {
    // `claim_token` rejects null `eb`; empty window / token exhaustion share
    // the free path. Callbacks are required by `wifi_init` (`arm_zero_copy`).
    if !window.is_empty()
        && let Some(token) = claim_token(eb)
    {
        let rc = unsafe {
            include::esp_wifi_internal_tx_by_ref(
                ifx,
                window.as_mut_ptr().cast(),
                window.len(),
                (token as *const RxBufferOwnership).cast_mut().cast(),
            )
        };
        // Drop the RX callback's reference; TX holds its own via netstack_ref.
        // On handoff failure Wi-Fi should balance any ref it took; this release
        // frees `eb` when refs hit 0.
        token.release();
        return rc == include::ESP_OK as i32;
    }

    super::xdp::free_rx_buffer(eb);
    false
}

fn claim_token(eb: *mut c_void) -> Option<&'static RxBufferOwnership> {
    TX_TOKENS.iter().find(|token| token.claim(eb))
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
        error!("xdp: Wi-Fi released an unknown TX token");
    }
}

fn token_from_raw(ptr: *mut c_void) -> Option<&'static RxBufferOwnership> {
    let ptr = ptr.cast::<RxBufferOwnership>();
    TX_TOKENS
        .iter()
        .find(|token| core::ptr::eq(*token as *const RxBufferOwnership, ptr.cast_const()))
}
