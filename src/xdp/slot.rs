//! Immutable executable-slot registration.

use anyhow::{Result, bail};

#[derive(Clone, Copy)]
pub struct Slot {
    base: usize,
    len: usize,
    primary: bool,
}
impl Slot {
    /// `base..base + len` must name a writable, executable, four-byte-aligned
    /// static region for the firmware's entire lifetime.
    pub const unsafe fn at(base: usize, len: usize, primary: bool) -> Self {
        Self { base, len, primary }
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
    pub(super) fn validate(self) -> Result<()> {
        if self.base == 0 || self.len == 0 {
            bail!("XDP slot must be non-empty");
        }
        if self.base & 3 != 0 || self.len & 3 != 0 {
            bail!("XDP slot must be four-byte aligned");
        }
        self.base
            .checked_add(self.len)
            .ok_or_else(|| anyhow::anyhow!("XDP slot address overflows"))?;
        Ok(())
    }
}
