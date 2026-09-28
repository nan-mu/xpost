//! Datapath / helper / pool error counters (no log on hot path).
//!
//! Control-plane failures stay as `Result`. Sites that cannot propagate
//! (RX ingress, BPF helper ABI, `Drop`) increment these counters instead.

use portable_atomic::{AtomicUsize, Ordering};

/// Process-wide soft-error counters for paths that cannot return `Result`.
pub struct ErrorCounters {
    /// `ingress::run` saw `!ACTIVE` or `PRIMARY_ENTRY == 0`.
    pub inactive_skip: AtomicUsize,
    /// `ingress::run` saw `IN_FLIGHT != 0` (reentrancy).
    pub reentry: AtomicUsize,
    /// `invoke_aot` stack not 16-byte aligned → forced PASS.
    pub stack_misalign: AtomicUsize,
    /// `XdpProg::run` rejected md window (bad data/len).
    pub bad_md: AtomicUsize,
    /// Helper map path: unresolved AOT map pointer.
    pub helper_unresolved: AtomicUsize,
    /// Helper map path: pool busy / unsupported / other map op soft-fail.
    pub helper_map: AtomicUsize,
    /// Pool `Drop` / reclaim saw misaligned or null ptr (leak path).
    pub pool_drop_misalign: AtomicUsize,
}

impl ErrorCounters {
    const fn new() -> Self {
        Self {
            inactive_skip: AtomicUsize::new(0),
            reentry: AtomicUsize::new(0),
            stack_misalign: AtomicUsize::new(0),
            bad_md: AtomicUsize::new(0),
            helper_unresolved: AtomicUsize::new(0),
            helper_map: AtomicUsize::new(0),
            pool_drop_misalign: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn snapshot(&self) -> ErrorCountersSnapshot {
        ErrorCountersSnapshot {
            inactive_skip: self.inactive_skip.load(Ordering::Relaxed),
            reentry: self.reentry.load(Ordering::Relaxed),
            stack_misalign: self.stack_misalign.load(Ordering::Relaxed),
            bad_md: self.bad_md.load(Ordering::Relaxed),
            helper_unresolved: self.helper_unresolved.load(Ordering::Relaxed),
            helper_map: self.helper_map.load(Ordering::Relaxed),
            pool_drop_misalign: self.pool_drop_misalign.load(Ordering::Relaxed),
        }
    }
}

/// Relaxed snapshot of [`ErrorCounters`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ErrorCountersSnapshot {
    pub inactive_skip: usize,
    pub reentry: usize,
    pub stack_misalign: usize,
    pub bad_md: usize,
    pub helper_unresolved: usize,
    pub helper_map: usize,
    pub pool_drop_misalign: usize,
}

/// Global soft-error counters.
pub static ERRORS: ErrorCounters = ErrorCounters::new();

#[inline]
pub(crate) fn inc_inactive_skip() {
    ERRORS.inactive_skip.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn inc_reentry() {
    ERRORS.reentry.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn inc_stack_misalign() {
    ERRORS.stack_misalign.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn inc_bad_md() {
    ERRORS.bad_md.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn inc_helper_unresolved() {
    ERRORS.helper_unresolved.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn inc_helper_map() {
    ERRORS.helper_map.fetch_add(1, Ordering::Relaxed);
}

#[inline]
pub(crate) fn inc_pool_drop_misalign() {
    ERRORS.pool_drop_misalign.fetch_add(1, Ordering::Relaxed);
}
