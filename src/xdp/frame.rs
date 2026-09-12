//! Saved by the XDP runtime before `jalr` into the leased entry.
//!
//! Must include RV32 callee-saved regs (`s0`–`s11`) for N3 restore on return.

/// Invoke call frame for native XDP entry (`jalr` / return restore).
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
