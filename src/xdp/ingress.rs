//! RX ingress: preprocess, run leased XDP under a critical section, postprocess.
//!
//! Radio TX/free and action dispatch live outside this module so the CS only
//! covers AOT flight state ([`crate::prog_table::XDP_FRAME`], pending redirect,
//! tail depth).
//!
//! **Stack contract (E3):** AOT runs on the **shared caller stack** (Wi-Fi /
//! embassy callback). There is no dedicated `XDP_STACK` switch. The firmware
//! wrapper saves callee-saved regs + return path in [`crate::prog_table::XDP_FRAME`]
//! and requires 16-byte ILP32 stack alignment at the `jalr` boundary. Peak
//! growth `A` (AOT + sync helpers) must satisfy `C + A + I <= S` from the
//! deployment receipt; canaries detect overflow but do not prove N2/N3.
//!
//! Enable Cargo feature `dev` so host `cargo check` / rust-analyzer typecheck this
//! path (RV32 asm is stubbed). Real asm: `cargo check-c3`.

use core::sync::atomic::Ordering;

use super::state::{ACTIVE, IN_FLIGHT, PRIMARY_ENTRY};

/// eBPF per-program stack budget (matches prevail `--stack-size` default).
/// Used for documentation / canary spacing — not as a private stack size.
pub(crate) const BPF_STACK_BYTES: usize = 512;
/// Extra room expected for synchronous helpers on the same caller stack.
pub(crate) const HELPER_STACK_MARGIN: usize = 512;
/// Conservative AOT+helper peak assumed until stack-usage reports land.
pub(crate) const AOT_STACK_PEAK_BYTES: usize = BPF_STACK_BYTES + HELPER_STACK_MARGIN;

/// Soft canary words below the current `sp` at invoke entry (debug/hwtest).
#[cfg(any(target_arch = "riscv32", feature = "dev"))]
const CANARY_WORDS: usize = 8;
#[cfg(any(target_arch = "riscv32", feature = "dev"))]
const CANARY_MAGIC: u32 = 0xA07_CAFE;

/// True when an XDP image is leased for RX.
#[inline]
pub(crate) fn is_leased() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

#[inline]
fn primary_entry() -> Option<usize> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    let entry = PRIMARY_ENTRY.load(Ordering::Acquire);
    (entry != 0).then_some(entry)
}

/// Run the leased program on `data`.
///
/// - **Pre (CS外):** fast lease check, build `xdp_md`
/// - **CS内:** re-validate entry, clear pending, reset tail depth, `jalr` on
///   the shared caller stack, restore callee-saved regs
/// - **Post (CS外):** snapshot md window for TX clipping
///
/// Returns `None` when not leased, raced with release, or reentrancy is
/// detected (`IN_FLIGHT != 0` at entry). Caller treats as PASS.
pub(crate) fn run(data: &mut [u8]) -> Option<i32> {
    let _ = primary_entry()?;

    #[cfg(any(target_arch = "riscv32", feature = "dev"))]
    {
        return run_flight(data);
    }

    #[cfg(not(any(target_arch = "riscv32", feature = "dev")))]
    {
        let _ = data;
        Some(crate::redirect::XDP_PASS)
    }
}

#[cfg(any(target_arch = "riscv32", feature = "dev"))]
fn run_flight(data: &mut [u8]) -> Option<i32> {
    use crate::ctx::XdpContext;

    let mut ctx = preprocess(data);
    let ctx_ptr = &mut ctx as *mut XdpContext;

    let action = critical_section::with(|cs| {
        if !ACTIVE.load(Ordering::Relaxed) {
            return None;
        }
        let entry = PRIMARY_ENTRY.load(Ordering::Relaxed);
        if entry == 0 {
            return None;
        }
        // Fail closed on reentrancy: at most one XDP flight (E4).
        if IN_FLIGHT.load(Ordering::Relaxed) != 0 {
            return None;
        }

        IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
        crate::redirect::clear_pending_with(cs);
        crate::prog_table::reset_tail_depth();
        let result = unsafe { invoke_aot(entry, ctx_ptr) };
        IN_FLIGHT.fetch_sub(1, Ordering::Release);
        Some(result)
    })?;

    postprocess(&ctx);
    Some(action)
}

/// Build packet context before entering the AOT critical section.
#[cfg(any(target_arch = "riscv32", feature = "dev"))]
#[inline]
fn preprocess(data: &mut [u8]) -> crate::ctx::XdpMd {
    crate::ctx::XdpMd::from_buf(data)
}

/// Snapshot `data`/`data_end` after AOT so TX can clip to the adjusted window.
#[cfg(any(target_arch = "riscv32", feature = "dev"))]
#[inline]
fn postprocess(ctx: &crate::ctx::XdpMd) {
    crate::ctx::record_window(ctx);
}

/// Firmware invoke frame on the **shared caller stack**: save return path and
/// callee-saved regs, keep current `sp` (must be 16-byte aligned), `jalr` into
/// the leased image, then restore.
///
/// AOT still emits its own BPF prologue/epilogue on top of this stack.
///
/// # Safety
/// `entry` must be a live IRAM XDP entry; `ctx` must remain valid for the call.
/// Caller must hold a critical section for the whole call (protects `XDP_FRAME`).
#[cfg(target_arch = "riscv32")]
#[inline(always)]
unsafe fn invoke_aot(entry: usize, ctx: *mut crate::ctx::XdpContext) -> i32 {
    let frame = core::ptr::addr_of_mut!(crate::prog_table::XDP_FRAME);
    // Soft canary: paint words just below current sp before jalr.
    paint_canary();
    let result: i32;
    unsafe {
        core::arch::asm!(
            // —— firmware prologue (shared stack; no sp switch) ——
            "mv t0, sp",
            "andi t1, t0, 15",
            "bnez t1, 2f",           // misaligned → force PASS path via sentinel
            "lla t1, 1f",
            "sw t0, 0({frame})",     // save caller sp for N3 / tail-call restore
            "sw t1, 4({frame})",
            "sw s0, 8({frame})",
            "sw s1, 12({frame})",
            "sw s2, 16({frame})",
            "sw s3, 20({frame})",
            "sw s4, 24({frame})",
            "sw s5, 28({frame})",
            "sw s6, 32({frame})",
            "sw s7, 36({frame})",
            "sw s8, 40({frame})",
            "sw s9, 44({frame})",
            "sw s10, 48({frame})",
            "sw s11, 52({frame})",
            "jalr {entry}",
            // —— firmware epilogue ——
            "1:",
            "lw s0, 8({frame})",
            "lw s1, 12({frame})",
            "lw s2, 16({frame})",
            "lw s3, 20({frame})",
            "lw s4, 24({frame})",
            "lw s5, 28({frame})",
            "lw s6, 32({frame})",
            "lw s7, 36({frame})",
            "lw s8, 40({frame})",
            "lw s9, 44({frame})",
            "lw s10, 48({frame})",
            "lw s11, 52({frame})",
            "lw sp, 0({frame})",
            "j 3f",
            "2:",
            "li a0, 2",              // XDP_PASS on alignment fault
            "3:",
            frame = in(reg) frame,
            entry = in(reg) entry,
            in("a0") ctx,
            lateout("a0") result,
            out("t0") _,
            out("t1") _,
            clobber_abi("C")
        );
    }
    debug_assert!(check_canary(), "XDP shared-stack canary corrupted");
    result
}

#[cfg(target_arch = "riscv32")]
#[inline(always)]
fn paint_canary() {
    let mut sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    // Paint downward from (sp - 16) so we do not clobber the red zone / args.
    let mut p = (sp.saturating_sub(16 + CANARY_WORDS * 4)) as *mut u32;
    for _ in 0..CANARY_WORDS {
        unsafe {
            core::ptr::write_volatile(p, CANARY_MAGIC);
            p = p.add(1);
        }
    }
}

#[cfg(target_arch = "riscv32")]
#[inline(always)]
fn check_canary() -> bool {
    let mut sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    let mut p = (sp.saturating_sub(16 + CANARY_WORDS * 4)) as *const u32;
    for _ in 0..CANARY_WORDS {
        let v = unsafe { core::ptr::read_volatile(p) };
        if v != CANARY_MAGIC {
            return false;
        }
        p = unsafe { p.add(1) };
    }
    true
}

/// Host stub so `--features dev` typechecks the flight path without RV32 asm.
#[cfg(all(feature = "dev", not(target_arch = "riscv32")))]
unsafe fn invoke_aot(entry: usize, ctx: *mut crate::ctx::XdpContext) -> i32 {
    let _ = (entry, ctx, AOT_STACK_PEAK_BYTES);
    crate::redirect::XDP_PASS
}
