//! RX ingress: build `xdp_md`, run leased XDP under a critical section, return action/window.
//!
//! Action mapping and Wi-Fi attach live in [`esp_radio`]; zero-copy Tx ownership
//! is handled by esp-radio's `xdp` feature.
//!
//! **Stack contract (E3):** the XDP image runs on the **shared caller stack**
//! (Wi-Fi / embassy callback). There is no dedicated `XDP_STACK` switch. The
//! firmware wrapper saves callee-saved regs + return path in [`XDP_FRAME`]
//! and requires 16-byte ILP32 stack alignment at the `jalr` boundary. Peak
//! growth `A` (image + sync helpers) must satisfy `C + A + I <= S` from the
//! deployment receipt; canaries detect overflow but do not prove N2/N3.
//!
//! This crate targets RV32 only; typecheck with `cargo check` / `cargo check-c3`
//! (root `.cargo/config.toml` sets `--target riscv32imc-unknown-none-elf`).

mod action;
mod stack;

pub(crate) mod ctx;
pub(crate) mod esp_radio;

use core::sync::atomic::Ordering;

use crate::xdp::{
    ingress::ctx::XdpMd,
    state::{ACTIVE, IN_FLIGHT, PRIMARY_ENTRY},
};

/// Invoke scratch in `.bss`: `[sp, ra, s0..s11]` (14 × `usize`), byte offsets 0..52.
/// Written/read only from RV32 asm in [`invoke_aot`]; CS covers the flight.
#[unsafe(link_section = ".bss")]
static mut XDP_FRAME: [usize; 14] = [0; 14];

/// Final packet range selected by an XDP program.
#[derive(Clone, Copy)]
pub(crate) struct PacketWindow {
    pub(crate) data: *mut u8,
    pub(crate) len: usize,
}

/// Result of one XDP flight.  Keeping the packet window with the action avoids
/// a global post-flight scratch slot being overwritten by a later RX callback.
#[derive(Clone, Copy)]
pub(crate) struct RunResult {
    pub(crate) action: i32,
    pub(crate) window: PacketWindow,
}

/// Run the leased program on `data`.
///
/// - **Pre (Out of CS):** build `xdp_md`
/// - **In CS:** validate lease/entry, `jalr` on the shared caller stack, restore
///   callee-saved regs
/// - **Post (Out of CS):** return the final md window with the action
///
/// Returns `None` when not leased, raced with release, or reentrancy is
/// detected (`IN_FLIGHT != 0` at entry). Caller treats as PASS.
pub(crate) fn run(data: &mut [u8]) -> Option<RunResult> {
    let mut ctx_md = XdpMd::from_buf(data);
    let ctx_ptr = &mut ctx_md as *mut XdpMd;

    let action = critical_section::with(|_cs| {
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
        let result = unsafe { invoke_aot(entry, ctx_ptr) };
        IN_FLIGHT.fetch_sub(1, Ordering::Release);
        Some(result)
    })?;

    let data = ctx_md.data;
    let len = (ctx_md.data_end as usize).checked_sub(data as usize)?;
    Some(RunResult {
        action,
        window: PacketWindow { data, len },
    })
}

/// Firmware invoke frame on the **shared caller stack**: save return path and
/// callee-saved regs, keep current `sp` (must be 16-byte aligned), `jalr` into
/// the leased image, then restore.
///
/// AOT still emits its own BPF prologue/epilogue on top of this stack.
///
/// `XDP_FRAME` is addressed via `lla t2, …` **before and after** `jalr`. An
/// `in(reg)` frame pointer does not survive the call: LLVM may place it in a
/// caller-saved reg (observed: `a2`), which the image is free to clobber.
///
/// # Safety
/// `entry` must be a live IRAM XDP entry; `ctx` must remain valid for the call.
/// Caller must hold a critical section for the whole call (protects `XDP_FRAME`).
#[inline(always)]
unsafe fn invoke_aot(entry: usize, ctx: *mut ctx::XdpMd) -> i32 {
    // Soft canary: paint words just below current sp before jalr.
    stack::paint_canary();
    let result: i32;
    unsafe {
        core::arch::asm!(
            // —— firmware prologue (shared stack; no sp switch) ——
            "mv t0, sp",
            "andi t1, t0, 15",
            "bnez t1, 2f",           // misaligned → force PASS path via sentinel
            "lla t1, 1f",
            "lla t2, {frame_sym}",
            "sw t0, 0(t2)",          // save caller sp for N3 return restore
            "sw t1, 4(t2)",          // unused after jalr (ra set by jalr); kept for frame layout
            "sw s0, 8(t2)",
            "sw s1, 12(t2)",
            "sw s2, 16(t2)",
            "sw s3, 20(t2)",
            "sw s4, 24(t2)",
            "sw s5, 28(t2)",
            "sw s6, 32(t2)",
            "sw s7, 36(t2)",
            "sw s8, 40(t2)",
            "sw s9, 44(t2)",
            "sw s10, 48(t2)",
            "sw s11, 52(t2)",
            "jalr {entry}",
            // —— firmware epilogue ——
            "1:",
            // Rematerialize: t2 is caller-saved and dead after jalr.
            "lla t2, {frame_sym}",
            "lw s0, 8(t2)",
            "lw s1, 12(t2)",
            "lw s2, 16(t2)",
            "lw s3, 20(t2)",
            "lw s4, 24(t2)",
            "lw s5, 28(t2)",
            "lw s6, 32(t2)",
            "lw s7, 36(t2)",
            "lw s8, 40(t2)",
            "lw s9, 44(t2)",
            "lw s10, 48(t2)",
            "lw s11, 52(t2)",
            "lw sp, 0(t2)",
            "j 3f",
            "2:",
            "li a0, 2",              // XDP_PASS on alignment fault
            "3:",
            frame_sym = sym XDP_FRAME,
            entry = in(reg) entry,
            in("a0") ctx,
            lateout("a0") result,
            out("t0") _,
            out("t1") _,
            out("t2") _,
            clobber_abi("C")
        );
    }
    debug_assert!(stack::check_canary(), "XDP shared-stack canary corrupted");
    result
}
