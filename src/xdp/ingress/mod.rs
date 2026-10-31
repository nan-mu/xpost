//! RX ingress: build `xdp_md`, run leased XDP as the single `IN_FLIGHT` owner.
//!
//! Wi-Fi attach / [`XdpAction`](xpost_type::XdpAction) → driver Action mapping
//! live in esp-radio; this module only owns the lease flight.
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

mod prog;
mod stack;

pub(crate) mod ctx;
#[cfg(feature = "esp-radio")]
pub(crate) mod esp_radio;

pub(crate) use prog::XDP_PROG;
#[cfg(any(
    feature = "perf-counter",
    feature = "perf-aggregate",
    feature = "perf-gauge",
    feature = "perf-trace"
))]
pub(crate) use prog::XdpProg;

use core::sync::atomic::Ordering;

use crate::xdp::{
    ingress::ctx::XdpMd,
    state::{ACTIVE, IN_FLIGHT, PRIMARY_ENTRY},
};

/// Invoke scratch in `.bss`: `[sp, ra, s0..s11]` (14 × `usize`), byte offsets 0..52.
/// Written/read only from RV32 asm in [`inner_xdp`]; the `IN_FLIGHT` owner has it.
#[unsafe(link_section = ".bss")]
static mut XDP_FRAME: [usize; 14] = [0; 14];

/// Final packet range selected by an XDP program.
#[derive(Clone, Copy)]
struct PacketWindow {
    data: *mut u8,
    len: usize,
}

/// Result of one inner flight (action code + md window).
#[derive(Clone, Copy)]
struct RunResult {
    action: i32,
    window: PacketWindow,
}

/// Lease flight for the crate-mounted [`XdpProg`](prog::XdpProg).
///
/// Returns Linux-style [`XdpAction`](xpost_type::XdpAction) + window. Driver
/// maps to Pass/Drop/Tx. A clear [`ACTIVE`] flag passes the frame (feature off).
/// After [`ACTIVE`] is set, a flight fault counts a runtime error and returns
/// [`XdpAction::Drop`].
#[esp_hal::ram]
pub(crate) fn run<'a>(frame: &'a mut [u8]) -> (xpost_type::XdpAction, &'a mut [u8]) {
    use xpost_type::XdpAction;

    if !ACTIVE.load(Ordering::Acquire) {
        return (XdpAction::Pass, frame);
    }
    let Some(result) = xdp(frame) else {
        return (XdpAction::Drop, frame);
    };
    let act = XdpAction::from(result.action);
    let window = result.window;
    let valid = (window.data.cast_const() == frame.as_ptr()) & (window.len <= frame.len());
    if !valid {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_bad_md();
        return (XdpAction::Drop, frame);
    }
    (act, &mut frame[..window.len])
}

/// Claim `IN_FLIGHT`, validate lease/entry, [`inner_xdp`] the image, return md window.
///
/// Returns `None` when the flight cannot start or the md length is corrupt.
/// Each path bumps a runtime-error counter. Caller drops the frame.
#[esp_hal::ram]
fn xdp(data: &mut [u8]) -> Option<RunResult> {
    let mut ctx_md = XdpMd::from_buf(data);
    let ctx_ptr = &mut ctx_md as *mut XdpMd;

    // Fail closed on reentrancy: at most one XDP flight (E4); owning IN_FLIGHT
    // also guards `XDP_FRAME`. IRQs stay enabled: tens of µs with interrupts off
    // per packet makes the Wi-Fi MAC miss TX deadlines (retries → ~100 ms stalls).
    if IN_FLIGHT
        .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
        .is_err()
    {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_reentry();
        return None;
    }
    // Dekker pair with `XdpRt::release` (ACTIVE=false → wait IN_FLIGHT==0):
    // either we see ACTIVE=false here, or release waits for this flight.
    let entry = if ACTIVE.load(Ordering::SeqCst) {
        PRIMARY_ENTRY.load(Ordering::SeqCst)
    } else {
        0
    };
    if entry == 0 {
        IN_FLIGHT.store(0, Ordering::Release);
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_inactive_skip();
        return None;
    }
    let action = unsafe { inner_xdp(entry, ctx_ptr) };
    IN_FLIGHT.store(0, Ordering::Release);

    let data = ctx_md.data;
    let Some(len) = (ctx_md.data_end as usize).checked_sub(data as usize) else {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_bad_md();
        return None;
    };
    Some(RunResult {
        action,
        window: PacketWindow { data, len },
    })
}

/// Firmware invoke on the **shared caller stack**: save return path and
/// callee-saved regs, keep current `sp` (must be 16-byte aligned), `jalr` into
/// the leased image, then restore.
///
/// # Safety
/// `entry` must be a live IRAM XDP entry; `ctx` must remain valid for the call.
/// Caller must own `IN_FLIGHT` for the whole call (protects `XDP_FRAME`).
#[inline(always)]
unsafe fn inner_xdp(entry: usize, ctx: *mut ctx::XdpMd) -> i32 {
    stack::paint_canary();

    let sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    if sp & 15 != 0 {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_stack_misalign();
        return xpost_type::XdpAction::Drop as i32;
    }

    let result: i32;
    unsafe {
        core::arch::asm!(
            "mv t0, sp",
            "lla t1, 1f",
            "lla t2, {frame_sym}",
            "sw t0, 0(t2)",
            "sw t1, 4(t2)",
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
            "1:",
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
