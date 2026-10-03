//! EspRadioTrace notify helper; trait lives in [`xpost_type::EspRadioTrace`].

/// Invoke a trace method on the mounted prog when `xdp-observe` is enabled.
#[inline(always)]
pub(crate) fn notify(f: impl FnOnce(&dyn super::EspRadioTrace)) {
    #[cfg(feature = "xdp-observe")]
    f(super::current());
    #[cfg(not(feature = "xdp-observe"))]
    let _ = f;
}
