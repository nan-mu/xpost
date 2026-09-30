//! Dormant `prof on` backends (not formal product exits).
//!
//! Timeline stamps live on [`crate::ext::perf::trace`] rings. These modules only
//! accumulate site µs / PC hits when `perf-span` / `perf-pchit` are enabled —
//! unused in current board profiles.

#[cfg(feature = "perf-span")]
pub mod span;
#[cfg(feature = "perf-pchit")]
pub mod pchit;

/// Enable/disable span + PC-hit sampling (`prof on|off`).
pub fn set_enabled(on: bool) {
    #[cfg(feature = "perf-span")]
    span::set_enabled(on);
    #[cfg(feature = "perf-pchit")]
    pchit::set_enabled(on);
    let _ = on;
}

#[cfg(feature = "perf-span")]
#[inline]
pub fn span() -> &'static span::SpanSet {
    &crate::xdp::ingress::XDP_PROG.span
}

#[cfg(feature = "perf-pchit")]
#[inline]
pub fn pchit() -> &'static pchit::PcHitSet {
    &crate::xdp::ingress::XDP_PROG.pchit
}
