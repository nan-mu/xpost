//! Pending XDP redirect target + TX frame side channel.
//!
//! Helpers set the ifindex; the WiFi RX path queues the frame for egress when
//! AOT returns `XDP_TX` / `XDP_REDIRECT`. Only [`IFINDEX_WIFI`] is implemented
//! on ESP32-C3; `IFINDEX_WIRED` (1) is a reserved stub (`-ENOTSUP` from helpers).

use core::cell::RefCell;

use critical_section::Mutex;
use heapless::Vec;

use crate::patch::ctx::IFINDEX_WIFI;

/// Sentinel: no pending redirect.
const NONE: u32 = u32::MAX;

/// Max Ethernet frame we will bounce for XDP_TX / REDIRECT.
const PENDING_FRAME_CAP: usize = 1600;

static PENDING_IFINDEX: Mutex<RefCell<u32>> = Mutex::new(RefCell::new(NONE));
static PENDING_FRAME: Mutex<RefCell<Option<Vec<u8, PENDING_FRAME_CAP>>>> =
    Mutex::new(RefCell::new(None));

/// Linux `enum xdp_action` values used by AOT / helpers.
pub const XDP_ABORTED: i32 = 0;
pub const XDP_DROP: i32 = 1;
pub const XDP_PASS: i32 = 2;
pub const XDP_TX: i32 = 3;
pub const XDP_REDIRECT: i32 = 4;

/// Name for AOT / driver log lines (`enum xdp_action`).
#[inline]
pub fn xdp_action_name(action: i32) -> &'static str {
    match action {
        XDP_ABORTED => "XDP_ABORTED",
        XDP_DROP => "XDP_DROP",
        XDP_PASS => "XDP_PASS",
        XDP_TX => "XDP_TX",
        XDP_REDIRECT => "XDP_REDIRECT",
        _ => "XDP_UNKNOWN",
    }
}

/// Record redirect destination before returning [`XDP_REDIRECT`].
#[inline]
pub fn set_pending(ifindex: u32) {
    critical_section::with(|cs| {
        *PENDING_IFINDEX.borrow_ref_mut(cs) = ifindex;
    });
}

/// Take and clear the pending redirect ifindex (if any).
#[inline]
pub fn take_pending() -> Option<u32> {
    critical_section::with(|cs| {
        let mut slot = PENDING_IFINDEX.borrow_ref_mut(cs);
        let v = *slot;
        *slot = NONE;
        if v == NONE { None } else { Some(v) }
    })
}

/// Clear without reading (e.g. before each XDP invoke).
#[inline]
pub fn clear_pending() {
    critical_section::with(|cs| {
        *PENDING_IFINDEX.borrow_ref_mut(cs) = NONE;
    });
}

/// Queue a frame for wifi egress (XDP_TX / supported REDIRECT).
///
/// Depth 1: returns `false` if the slot is occupied or `buf` is too large.
#[inline]
pub fn queue_egress_frame(buf: &[u8]) -> bool {
    if buf.len() > PENDING_FRAME_CAP {
        return false;
    }
    let mut v = Vec::new();
    if v.extend_from_slice(buf).is_err() {
        return false;
    }
    critical_section::with(|cs| {
        let mut slot = PENDING_FRAME.borrow_ref_mut(cs);
        if slot.is_some() {
            return false;
        }
        *slot = Some(v);
        true
    })
}

/// Take a queued egress frame, if any.
#[inline]
pub fn take_egress_frame() -> Option<Vec<u8, PENDING_FRAME_CAP>> {
    critical_section::with(|cs| PENDING_FRAME.borrow_ref_mut(cs).take())
}

/// True if `ifindex` has an implemented egress path (wifi only on C3).
///
/// `IFINDEX_WIRED` (1) is reserved for multi-NIC SoCs; callers get `-ENOTSUP`.
#[inline]
pub fn ifindex_supported(ifindex: u32) -> bool {
    ifindex == IFINDEX_WIFI
}
