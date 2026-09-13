//! Soft stack canary on the shared caller stack (debug / hwtest).
//!
//! Does **not** reserve stack for the XDP image, evaluate E3
//! (`C + A + I <= S`), or prove N2/N3.

use crate::xdp::state::{
    STACK_CANARY_GAP_BYTES, STACK_CANARY_MAGIC, STACK_CANARY_WORDS,
};

/// Soft stack probe before `jalr` into the XDP image.
///
/// Reads the current `sp`, skips [`STACK_CANARY_GAP_BYTES`] below it, then
/// paints [`STACK_CANARY_WORDS`] words with [`STACK_CANARY_MAGIC`]. The XDP
/// image (and sync helpers) grow the **shared caller stack** downward; if they
/// smash this band, [`check_canary`] fails. This does **not** reserve stack,
/// enforce E3 (`C + A + I <= S`), or prove N2/N3.
#[inline(always)]
pub(super) fn paint_canary() {
    let mut sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    let mut p =
        (sp.saturating_sub(STACK_CANARY_GAP_BYTES + STACK_CANARY_WORDS * 4)) as *mut u32;
    for _ in 0..STACK_CANARY_WORDS {
        unsafe {
            core::ptr::write_volatile(p, STACK_CANARY_MAGIC);
            p = p.add(1);
        }
    }
}

/// `true` iff the band painted by [`paint_canary`] is still intact.
///
/// Called after the XDP image returns (via `debug_assert!` only). A mismatch
/// means something wrote through the probe band on the shared stack; release
/// builds typically strip the assert, so this is debug/hwtest signal only.
#[inline(always)]
pub(super) fn check_canary() -> bool {
    let mut sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    let mut p =
        (sp.saturating_sub(STACK_CANARY_GAP_BYTES + STACK_CANARY_WORDS * 4)) as *const u32;
    for _ in 0..STACK_CANARY_WORDS {
        let v = unsafe { core::ptr::read_volatile(p) };
        if v != STACK_CANARY_MAGIC {
            return false;
        }
        p = unsafe { p.add(1) };
    }
    true
}
