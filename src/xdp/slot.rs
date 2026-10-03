//! IRAM executable slots for XDP images.

use crate::error::XdpInitError;

/// Board-static IRAM buffer registered at boot (`Slot<N>` in examples).
pub struct Slot<const N: usize> {
    buf: [u8; N],
}

impl<const N: usize> Slot<N> {
    pub const fn new() -> Self {
        Self { buf: [0; N] }
    }

    pub fn as_xdp_slot(&'static mut self, primary: bool) -> XdpSlot {
        XdpSlot {
            base: self.buf.as_mut_ptr() as usize,
            len: N,
            primary,
        }
    }
}

impl<const N: usize> Default for Slot<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Descriptor for one IRAM slot (base + length + primary flag).
#[derive(Clone, Copy, Debug)]
pub struct XdpSlot {
    base: usize,
    len: usize,
    primary: bool,
}

impl XdpSlot {
    pub(crate) fn base(self) -> usize {
        self.base
    }

    pub(crate) fn len(self) -> usize {
        self.len
    }

    pub(crate) fn is_primary(self) -> bool {
        self.primary
    }

    pub(crate) fn validate(self) -> Result<(), XdpInitError> {
        if self.len == 0 {
            return Err(XdpInitError::EmptySlot);
        }
        if (self.base & 3) != 0 {
            return Err(XdpInitError::SlotAlignment);
        }
        if self.base.checked_add(self.len).is_none() {
            return Err(XdpInitError::SlotOverflow);
        }
        Ok(())
    }
}
