//! Process-wide XDP runtime state and fixed capacity limits.

use super::XdpRt;
use core::sync::atomic::{AtomicBool, AtomicUsize};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::once_lock::OnceLock;

pub(super) const MAX_SLOTS: usize = 4;
pub(super) const MAX_MAP_REFS: usize = 16;
pub static XDP_RT: OnceLock<Mutex<CriticalSectionRawMutex, XdpRt>> = OnceLock::new();
pub(super) static ACTIVE: AtomicBool = AtomicBool::new(false);
pub(super) static PRIMARY_ENTRY: AtomicUsize = AtomicUsize::new(0);
pub(super) static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
