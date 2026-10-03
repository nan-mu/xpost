//! XDP action constants + TX frame side channel.
//!
//! `bpf_redirect` / `bpf_redirect_map` helpers were removed. TX still uses the
//! egress queue when zero-copy `tx_by_ref` is unavailable.

use core::cell::RefCell;

use critical_section::Mutex;
use heapless::{Deque, Vec};

/// Max Ethernet frame we will bounce for XDP_TX.
const PENDING_FRAME_CAP: usize = 1600;
const EGRESS_QUEUE_DEPTH: usize = 16;

type EgressFrame = Vec<u8, PENDING_FRAME_CAP>;

static PENDING_FRAMES: Mutex<RefCell<Deque<EgressFrame, EGRESS_QUEUE_DEPTH>>> =
    Mutex::new(RefCell::new(Deque::new()));

/// Linux `enum xdp_action` values used by AOT / helpers.
pub const XDP_ABORTED: i32 = 0;
pub const XDP_DROP: i32 = 1;
pub const XDP_PASS: i32 = 2;
pub const XDP_TX: i32 = 3;
/// Retained for ABI compatibility; redirect helper is unsupported.
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

/// No-op: redirect pending ifindex was removed with redirect helpers.
#[inline]
pub(crate) fn clear_pending_with(_cs: critical_section::CriticalSection<'_>) {}

/// Clear redirect pending (no-op after redirect helper removal).
#[inline]
pub fn clear_pending() {}

fn copy_frame(buf: &[u8]) -> Option<EgressFrame> {
    if buf.len() > PENDING_FRAME_CAP {
        return None;
    }
    let mut v = Vec::new();
    v.extend_from_slice(buf).ok()?;
    Some(v)
}

/// Queue a frame for wifi egress (XDP_TX).
#[inline]
pub fn queue_egress_frame(buf: &[u8]) -> bool {
    let Some(v) = copy_frame(buf) else {
        return false;
    };
    critical_section::with(|cs| PENDING_FRAMES.borrow_ref_mut(cs).push_back(v).is_ok())
}

/// Put a taken frame back at the head (TX token unavailable).
#[inline]
pub fn requeue_egress_front(frame: EgressFrame) -> bool {
    critical_section::with(|cs| PENDING_FRAMES.borrow_ref_mut(cs).push_front(frame).is_ok())
}

/// Take the oldest queued egress frame, if any.
#[inline]
pub fn take_egress_frame() -> Option<EgressFrame> {
    critical_section::with(|cs| PENDING_FRAMES.borrow_ref_mut(cs).pop_front())
}
