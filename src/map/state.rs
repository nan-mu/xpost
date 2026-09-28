//! Map runtime singleton + capacity constants.

use embassy_sync::once_lock::OnceLock;

use crate::{RtMutex, map::MapRt};

/// Global map runtime singleton.
pub static MAP_RT: OnceLock<RtMutex<MapRt>> = OnceLock::new();

/// Whether talc heap was claimed during [`MapRt::init`].
pub(crate) static mut HEAP_CLAIMED: bool = false;

/// Max AOT VMA → map-index alias slots (lease path).
pub(crate) const MAX_MAP_ALIASES: usize = 16;
