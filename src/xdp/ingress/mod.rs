//! RX ingress: build `xdp_md`, run leased XDP under a critical section, record window.
//!
//! Radio TX/free and action dispatch ([`action`]) live beside invoke so the CS
//! only covers flight state ([`XDP_FRAME`]).
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

pub mod esp_radio;

use core::sync::atomic::Ordering;

use crate::xdp::state::{ACTIVE, IN_FLIGHT, PRIMARY_ENTRY};

/// Invoke scratch in `.bss`: `[sp, ra, s0..s11]` (14 × `usize`), byte offsets 0..52.
/// Written/read only from RV32 asm in [`invoke_aot`]; CS covers the flight.
#[unsafe(link_section = ".bss")]
static mut XDP_FRAME: [usize; 14] = [0; 14];

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
    run_flight(data)
}

fn run_flight(data: &mut [u8]) -> Option<i32> {
    use crate::ctx::XdpContext;

    let mut ctx = crate::ctx::XdpMd::from_buf(data);
    let ctx_ptr = &mut ctx as *mut XdpContext;

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

    crate::ctx::record_window(&ctx);
    Some(action)
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
#[inline(always)]
unsafe fn invoke_aot(entry: usize, ctx: *mut crate::ctx::XdpContext) -> i32 {
    let frame = core::ptr::addr_of_mut!(XDP_FRAME);
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
            "sw t0, 0({frame})",     // save caller sp for N3 return restore
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
    debug_assert!(stack::check_canary(), "XDP shared-stack canary corrupted");
    result
}
