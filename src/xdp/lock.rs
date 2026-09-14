//! Shared embassy_sync lock aliases for the XDP runtime.

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    mutex::{Mutex, MutexGuard},
};

pub type RtMutex<T> = Mutex<CriticalSectionRawMutex, T>;
pub type RtMutexGuard<'a, T> = MutexGuard<'a, CriticalSectionRawMutex, T>;
pub type XdpRtGuard<'a> = RtMutexGuard<'a, crate::xdp::XdpRt>;
