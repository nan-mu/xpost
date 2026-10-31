//! Map runtime singleton + capacity constants.

use embassy_sync::once_lock::OnceLock;
use portable_atomic::{AtomicBool, Ordering};

use crate::{RtMutex, map::MapRt};

/// Global map runtime singleton.
pub static MAP_RT: OnceLock<RtMutex<MapRt>> = OnceLock::new();

/// Control-plane reads and writes. Closed from a successful lease until
/// `XdpRt::release` has drained `IN_FLIGHT`.
static CTRL_OPEN: AtomicBool = AtomicBool::new(true);

pub(crate) fn close_ctrl() {
    CTRL_OPEN.store(false, Ordering::Release);
}

pub(crate) fn open_ctrl() {
    CTRL_OPEN.store(true, Ordering::Release);
}

pub(crate) fn ctrl_open() -> bool {
    CTRL_OPEN.load(Ordering::Acquire)
}

/// Whether talc heap was claimed during [`MapRt::init`].
pub(crate) static mut HEAP_CLAIMED: bool = false;

/// Max AOT VMA → map-index alias slots (lease path).
pub(crate) const MAX_MAP_ALIASES: usize = 16;
