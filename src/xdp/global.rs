//! All process-wide XDP state.
//!
//! Keeping these declarations together makes the lock-based control plane and
//! the lock-free RX snapshot auditable as one ownership boundary.

use core::sync::atomic::{AtomicBool, AtomicUsize};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::once_lock::OnceLock;

use super::XdpRt;

/// Library-owned XDP runtime. Its lock is intentionally the underlying
/// `OnceLock<Mutex<_>>`, rather than a wrapper type.
pub static XDP_RT: OnceLock<Mutex<CriticalSectionRawMutex, XdpRt>> = OnceLock::new();

/// Published only after a complete primary-image load and lease.
pub(super) static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Lock-free entry pointer read by the RX hot path.
pub(super) static PRIMARY_ENTRY: AtomicUsize = AtomicUsize::new(0);
/// RX calls that have acquired the primary snapshot.
pub(super) static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
