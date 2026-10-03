//! Immutable executable-slot registration.

use crate::error::XdpInitError;

#[derive(Clone, Copy)]
pub struct XdpSlot {
    base: usize,
    len: usize,
    primary: bool,
}

impl XdpSlot {
    /// # Safety
    ///
    /// `base..base + len` must name a writable, executable, four-byte-aligned
    /// static region for the firmware's entire lifetime.
    pub const unsafe fn from_raw_parts(base: usize, len: usize, primary: bool) -> Self {
        Self { base, len, primary }
    }

    /// Placeholder used only until overwritten during `init_primary`.
    pub const fn dangling_primary() -> Self {
        Self {
            base: 1,
            len: 4,
            primary: true,
        }
    }

    pub(crate) const fn base(self) -> usize {
        self.base
    }

    pub(crate) const fn len(self) -> usize {
        self.len
    }

    pub(crate) const fn is_primary(self) -> bool {
        self.primary
    }

    pub(crate) fn validate(self) -> Result<(), XdpInitError> {
        if self.base == 0 || self.len == 0 {
            return Err(XdpInitError::EmptySlot);
        }
        if self.base & 3 != 0 || self.len & 3 != 0 {
            return Err(XdpInitError::SlotAlignment);
        }
        if self.base.checked_add(self.len).is_none() {
            return Err(XdpInitError::SlotOverflow);
        }
        Ok(())
    }
}

/// Static IRAM buffer used by board examples (`link_section = .xdp.hotload_*`).
#[repr(C, align(4))]
pub struct Slot<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> Slot<N> {
    pub const fn new(bytes: [u8; N]) -> Self {
        Self { bytes }
    }

    /// Build an [`XdpSlot`] descriptor over this static buffer.
    ///
    /// # Safety
    /// `self` must remain at a fixed IRAM address for the firmware lifetime.
    pub unsafe fn as_xdp_slot(&'static self, primary: bool) -> XdpSlot {
        unsafe { XdpSlot::from_raw_parts(self.bytes.as_ptr() as usize, N, primary) }
    }
}
