//! Saved by the XDP runtime before `jalr` into the leased entry.
//!
//! Must include RV32 callee-saved regs (`s0`–`s11`) for N3 restore on return.
//! Written/read by `invoke_aot` asm.

/// Invoke call frame for native XDP entry (`jalr` / return restore).
#[repr(C)]
#[allow(dead_code)] // written/read only from RV32 asm in `invoke_aot`
pub(super) struct XdpCallFrame {
    pub(super) sp: usize,
    pub(super) ra: usize,
    pub(super) s0: usize,
    pub(super) s1: usize,
    pub(super) s2: usize,
    pub(super) s3: usize,
    pub(super) s4: usize,
    pub(super) s5: usize,
    pub(super) s6: usize,
    pub(super) s7: usize,
    pub(super) s8: usize,
    pub(super) s9: usize,
    pub(super) s10: usize,
    pub(super) s11: usize,
}

#[allow(dead_code)] // RV32 `invoke_aot` only
pub(super) static mut XDP_FRAME: XdpCallFrame = XdpCallFrame {
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

