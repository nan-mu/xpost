//! Re-export observe notify helper; trait lives in [`xpost_trait::Observe`].

/// Invoke an observe method on the mounted prog when `xdp-observe` is enabled.
#[inline(always)]
pub(crate) fn notify(f: impl FnOnce(&dyn super::XdpProg)) {
    #[cfg(feature = "xdp-observe")]
    f(super::current());
    #[cfg(not(feature = "xdp-observe"))]
    let _ = f;
}
