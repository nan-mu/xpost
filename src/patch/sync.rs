//! Patch-wide sync backends for pool / program slots / ctrl gate.
//!
//! # Features
//!
//! - `patch-sync-embassy` — `embassy_sync::rwlock::RwLock` (async control plane; **default**)
//! - `patch-sync-st` — `RefCell` (sync control plane)
//!
//! If both are enabled (e.g. `cargo build --features patch-sync-st` with default
//! features), **`patch-sync-st` wins**.
//!
//! Helper / AOT data plane always uses [`RwCell::try_write`] / [`RwCell::try_read`]
//! (never `.await`).
//!
//! # Safety (ST)
//!
//! Single-core cooperative tasks only. Interrupt-level code must not enter patch
//! helpers or talc while a [`RefCell`] borrow is active (same bound as `SyncTalc`).

#[cfg(not(any(feature = "patch-sync-embassy", feature = "patch-sync-st")))]
compile_error!("enable `patch-sync-embassy` (default) or `patch-sync-st`");

use core::cell::RefCell;

use critical_section::Mutex;

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

#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
mod backend {
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embassy_sync::rwlock::RwLock;

    type WriteGuard<'a, T> =
        embassy_sync::rwlock::RwLockWriteGuard<'a, CriticalSectionRawMutex, T>;
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
        #[allow(dead_code)]
        pub fn try_read(&self) -> Option<ReadGuard<'_, T>> {
            self.0.try_read().ok()
        }
    }
}

#[cfg(feature = "patch-sync-st")]
mod backend {
    use core::cell::{Ref, RefCell, RefMut};

    type WriteGuard<'a, T> = RefMut<'a, T>;
    type ReadGuard<'a, T> = Ref<'a, T>;

    /// Read/write cell: sync borrows; `try_*` maps to `RefCell::try_borrow*`.
    pub struct RwCell<T>(RefCell<T>);

    // SAFETY: single-core + no IRQ reentry into patch (see module docs).
    unsafe impl<T: Send> Sync for RwCell<T> {}

    impl<T> RwCell<T> {
        pub const fn new(value: T) -> Self {
            Self(RefCell::new(value))
        }

        #[inline]
        pub fn write(&self) -> WriteGuard<'_, T> {
            self.0.borrow_mut()
        }

        #[inline]
        pub fn read(&self) -> ReadGuard<'_, T> {
            self.0.borrow()
        }

        #[inline]
        pub fn try_write(&self) -> Option<WriteGuard<'_, T>> {
            self.0.try_borrow_mut().ok()
        }

        #[inline]
        #[allow(dead_code)]
        pub fn try_read(&self) -> Option<ReadGuard<'_, T>> {
            self.0.try_borrow().ok()
        }
    }
}

#[cfg(any(
    all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")),
    feature = "patch-sync-st",
))]
pub use backend::RwCell;
