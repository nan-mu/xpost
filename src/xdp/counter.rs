//! Datapath / helper / pool soft-fail counters (no log on hot path).
//!
//! When `trace-counter` is enabled, increments go to [`crate::TRACE`].
//! Otherwise they are no-ops (clean datapath).

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_inactive_skip() {
    crate::TRACE.inc_inactive_skip();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_reentry() {
    crate::TRACE.inc_reentry();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_stack_misalign() {
    crate::TRACE.inc_stack_misalign();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_bad_md() {
    crate::TRACE.inc_bad_md();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_helper_unresolved() {
    crate::TRACE.inc_helper_unresolved();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_helper_map() {
    crate::TRACE.inc_helper_map();
}

#[cfg(feature = "trace-counter")]
#[inline]
pub(crate) fn inc_pool_drop_misalign() {
    crate::TRACE.inc_pool_drop_misalign();
}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_inactive_skip() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_reentry() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_stack_misalign() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_bad_md() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_helper_unresolved() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_helper_map() {}

#[cfg(not(feature = "trace-counter"))]
#[inline]
pub(crate) fn inc_pool_drop_misalign() {}
