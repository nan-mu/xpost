//! Shared invoke call frame for native XDP entry (`jalr` / return restore).
//!
//! Hot-slot metadata used by optional `hot-switch` lives under that feature.

use super::mem_map::{HOT_SLOT_BASES, HOT_SLOT_COUNT, HOT_SLOT_LENS};

/// IRAM hot-load slot index (`0..HOT_SLOT_COUNT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct SlotId(pub u8);

impl SlotId {
    /// RX default entry (slot 0).
    pub const A: Self = Self(0);
    /// Second slot (multi-image / hot-switch builds).
    pub const B: Self = Self(1);

    pub fn from_u8(v: u8) -> Option<Self> {
        if (v as usize) < HOT_SLOT_COUNT {
            Some(Self(v))
        } else {
            None
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub fn base(self) -> usize {
        HOT_SLOT_BASES[self.index()]
    }

    pub fn len(self) -> usize {
        HOT_SLOT_LENS[self.index()]
    }

    /// Entry = slot base (linker `ENTRY(_start)` at region origin).
    pub fn entry(self) -> usize {
        self.base()
    }
}

/// Saved by the XDP runtime before `jalr` into the leased entry.
///
/// Must include RV32 callee-saved regs (`s0`–`s11`) for N3 restore on return.
#[repr(C)]
pub struct XdpCallFrame {
    pub sp: usize,
    pub ra: usize,
    pub s0: usize,
    pub s1: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
}

pub static mut XDP_FRAME: XdpCallFrame = XdpCallFrame {
    sp: 0,
    ra: 0,
    s0: 0,
    s1: 0,
    s2: 0,
    s3: 0,
    s4: 0,
    s5: 0,
    s6: 0,
    s7: 0,
    s8: 0,
    s9: 0,
    s10: 0,
    s11: 0,
};
