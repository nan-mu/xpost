//! TX frame bounce queue when zero-copy `tx_by_ref` is unavailable.

use core::cell::RefCell;

use critical_section::Mutex;
use heapless::{Deque, Vec};

/// Max Ethernet frame we will bounce for XDP_TX.
const PENDING_FRAME_CAP: usize = 1600;
const EGRESS_QUEUE_DEPTH: usize = 16;

type EgressFrame = Vec<u8, PENDING_FRAME_CAP>;

static PENDING_FRAMES: Mutex<RefCell<Deque<EgressFrame, EGRESS_QUEUE_DEPTH>>> =
    Mutex::new(RefCell::new(Deque::new()));

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
