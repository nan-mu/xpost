//! Patch-wide sync: embassy `RwLock` for pool / program slots / ctrl gate.
//!
//! Helper / AOT data plane uses [`RwCell::try_write`] / [`RwCell::try_read`]
//! (never `.await`).

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::rwlock::RwLock;

/// True while install/remove mutates ABI + DRAM (coarse gate).
static CTRL_BUSY: Mutex<RefCell<bool>> = Mutex::new(RefCell::new(false));

/// Acquire the program control-plane busy flag. `false` if already busy.
#[inline]
pub fn ctrl_try_begin() -> bool {
    critical_section::with(|cs| {
        let mut busy = CTRL_BUSY.borrow_ref_mut(cs);
        if *busy {
            return false;
        }
        *busy = true;
        true
    })
}

/// Release the program control-plane busy flag.
#[inline]
pub fn ctrl_end() {
    critical_section::with(|cs| {
        *CTRL_BUSY.borrow_ref_mut(cs) = false;
    });
}

type WriteGuard<'a, T> = embassy_sync::rwlock::RwLockWriteGuard<'a, CriticalSectionRawMutex, T>;
type ReadGuard<'a, T> = embassy_sync::rwlock::RwLockReadGuard<'a, CriticalSectionRawMutex, T>;

/// Read/write cell: async `write`/`read`, sync `try_*` for helpers.
pub struct RwCell<T>(RwLock<CriticalSectionRawMutex, T>);

impl<T> RwCell<T> {
    pub const fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }

    #[inline]
    pub async fn write(&self) -> WriteGuard<'_, T> {
        self.0.write().await
    }

    #[inline]
    pub async fn read(&self) -> ReadGuard<'_, T> {
        self.0.read().await
    }

    #[inline]
    pub fn try_write(&self) -> Option<WriteGuard<'_, T>> {
        self.0.try_write().ok()
    }

    #[inline]
    pub fn try_read(&self) -> Option<ReadGuard<'_, T>> {
        self.0.try_read().ok()
    }
}
