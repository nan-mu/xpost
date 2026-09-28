//! Observe notify helper; trait lives in [`xpost_type::Observe`].

/// Invoke an observe method on the mounted prog when `xdp-observe` is enabled.
#[inline(always)]
pub(crate) fn notify(f: impl FnOnce(&dyn super::Observe)) {
    #[cfg(feature = "xdp-observe")]
    f(super::current());
    #[cfg(not(feature = "xdp-observe"))]
    let _ = f;
}
