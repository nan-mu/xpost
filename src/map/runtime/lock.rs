//! Shared embassy_sync lock aliases for the map runtime.

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex,
    mutex::{Mutex, MutexGuard},
};

pub type RtMutex<T> = Mutex<CriticalSectionRawMutex, T>;
pub type RtMutexGuard<'a, T> = MutexGuard<'a, CriticalSectionRawMutex, T>;
pub type MapRtGuard<'a> = RtMutexGuard<'a, crate::map::MapRt>;
