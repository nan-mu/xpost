//! Process-wide XDP runtime state and fixed capacity limits.

use super::lock::RtMutex;
use crate::xdp::XdpRt;
use embassy_sync::once_lock::OnceLock;
use portable_atomic::{AtomicBool, AtomicUsize};

pub(crate) const MAX_SLOTS: usize = 4;
pub(crate) const MAX_MAP_REFS: usize = 16;
pub(crate) const MAX_IMAGES: usize = 8;
pub(crate) const MAX_VMR_BINDS: usize = 16;

pub static XDP_RT: OnceLock<RtMutex<XdpRt>> = OnceLock::new();
pub(crate) static ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static PRIMARY_ENTRY: AtomicUsize = AtomicUsize::new(0);
pub(crate) static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
