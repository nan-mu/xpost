//! Saved by the XDP runtime before `jalr` into the leased entry.
//!
//! Must include RV32 callee-saved regs (`s0`–`s11`) for N3 restore on return.

/// Invoke call frame for native XDP entry (`jalr` / return restore).
#[repr(C)]
pub(crate) struct XdpCallFrame {
    pub(crate) sp: usize,
    pub(crate) ra: usize,
    pub(crate) s0: usize,
    pub(crate) s1: usize,
    pub(crate) s2: usize,
    pub(crate) s3: usize,
    pub(crate) s4: usize,
    pub(crate) s5: usize,
    pub(crate) s6: usize,
    pub(crate) s7: usize,
    pub(crate) s8: usize,
    pub(crate) s9: usize,
    pub(crate) s10: usize,
    pub(crate) s11: usize,
}

pub(crate) static mut XDP_FRAME: XdpCallFrame = XdpCallFrame {
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
