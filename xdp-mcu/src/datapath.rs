//! XDP datapath activity gate for userspace hash/array access.
//!
//! While active, BPF may still `lookup` hash/array on the XDP path. Userspace
//! `with_hash_map` / `with_array_map` must see a stopped datapath (boot seed, or
//! after [`disable`] before mutating rules). Ringbuf userspace is not gated here.

use core::cell::RefCell;

use critical_section::Mutex;
use log::error;

use super::map::MapError;

static ACTIVE: Mutex<RefCell<bool>> = Mutex::new(RefCell::new(false));

/// Mark XDP RX / AOT datapath as running.
#[inline]
pub fn enable() {
    critical_section::with(|cs| {
        *ACTIVE.borrow_ref_mut(cs) = true;
    });
}

/// Mark datapath stopped so userspace may touch hash/array maps.
#[inline]
pub fn disable() {
    critical_section::with(|cs| {
        *ACTIVE.borrow_ref_mut(cs) = false;
    });
}

/// Whether the XDP datapath is considered active.
#[inline]
pub fn is_active() -> bool {
    critical_section::with(|cs| *ACTIVE.borrow_ref(cs))
}

/// Err([`MapError::Busy`]) if datapath is active (hash/array userspace forbidden).
pub fn require_stopped() -> Result<(), MapError> {
    if is_active() {
        error!("[userspace] hash/array requires XDP datapath stopped (call datapath::disable)");
        return Err(MapError::Busy);
    }
    Ok(())
}
