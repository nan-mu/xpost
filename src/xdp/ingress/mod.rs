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

use xpost_type::{RxVerdict, XdpAction};

use crate::xdp::{
    ingress::ctx::XdpMd,
    state::{
        ACTIVE, IN_FLIGHT, PAUSE, PRIMARY_ENGINE, PRIMARY_ENTRY, PRIMARY_LEN, QUIESCE_DROPS,
        QUIESCE_PASS, QUIESCE_PASSES,
    },
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
    verdict: RxVerdict,
}

/// Lease flight for the crate-mounted [`XdpProg`](prog::XdpProg).
///
/// Returns Linux-style [`XdpAction`](xpost_type::XdpAction) + window. Driver
/// maps to Pass/Drop/Tx. A clear [`ACTIVE`] flag passes the frame (feature off).
/// After [`ACTIVE`] is set, a flight fault returns [`XdpAction::Drop`].
/// A hot-update [`PAUSE`](crate::xdp::state::PAUSE) applies the quiesce policy
/// without entering either image. Those frames are counted in `get xdp`.
#[esp_hal::ram]
pub(crate) fn run(frame: &mut [u8]) -> (XdpAction, &mut [u8]) {
    let (action, window, _) = run_observed(frame);
    (action, window)
}

/// Observe the policy verdict while retaining the driver action and window.
#[esp_hal::ram]
pub(crate) fn run_observed(frame: &mut [u8]) -> (XdpAction, &mut [u8], RxVerdict) {
    if !ACTIVE.load(Ordering::Acquire) {
        return (XdpAction::Pass, frame, RxVerdict::Bypass);
    }
    let result = match xdp(frame) {
        Ok(result) => result,
        Err(verdict) => return (XdpAction::Drop, frame, verdict),
    };
    let act = XdpAction::from(result.action);
    let window = result.window;
    let valid = (window.data.cast_const() == frame.as_ptr()) & (window.len <= frame.len());
    if !valid {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_bad_md();
        return (XdpAction::Drop, frame, RxVerdict::Fault);
    }
    (act, &mut frame[..window.len], result.verdict)
}

/// Claim `IN_FLIGHT`, validate lease/entry, [`inner_xdp`] the image, return md window.
///
/// Return a fault when the flight cannot start or the packet metadata is invalid.
/// The caller drops faults and reports quiesce separately.
#[esp_hal::ram]
fn xdp(data: &mut [u8]) -> Result<RunResult, RxVerdict> {
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
        return Err(RxVerdict::Fault);
    }
    // Dekker pair with `XdpRt::activate`: PAUSE store, then wait IN_FLIGHT==0,
    // then store PRIMARY_ENTRY. A flight that passes this load runs the entry
    // sampled below; activate will not publish until it finishes.
    // A flight that observes PAUSE does not run either image. PASS and DROP
    // are counted apart from feature-off PASS and from a program XDP_DROP.
    if PAUSE.load(Ordering::SeqCst) {
        IN_FLIGHT.store(0, Ordering::Release);
        if QUIESCE_PASS.load(Ordering::SeqCst) {
            QUIESCE_PASSES.fetch_add(1, Ordering::Relaxed);
            return Ok(RunResult {
                action: XdpAction::Pass as i32,
                verdict: RxVerdict::Bypass,
                window: PacketWindow {
                    data: data.as_mut_ptr(),
                    len: data.len(),
                },
            });
        }
        QUIESCE_DROPS.fetch_add(1, Ordering::Relaxed);
        return Err(RxVerdict::Bypass);
    }
    // Dekker pair with `XdpRt::release` (ACTIVE=false → wait IN_FLIGHT==0):
    // either we see ACTIVE=false here, or release waits for this flight.
    // The engine triple is stored before ACTIVE, so one `ACTIVE` load gates all
    // three reads below.
    let (entry, engine) = if ACTIVE.load(Ordering::SeqCst) {
        let code = PRIMARY_ENGINE.load(Ordering::SeqCst);
        let engine = match code {
            crate::xdp::engine::Engine::AOT_CODE => crate::xdp::engine::Engine::Aot,
            #[cfg(feature = "interp")]
            crate::xdp::engine::Engine::INTERP_CODE => crate::xdp::engine::Engine::Interp {
                len: PRIMARY_LEN.load(Ordering::SeqCst),
            },
            // A code the flight does not understand fails closed rather than
            // guessing which engine produced the bytes.
            _ => {
                IN_FLIGHT.store(0, Ordering::Release);
                #[cfg(feature = "perf-counter")]
                crate::ext::perf::counter::inc_inactive_skip();
                return Err(RxVerdict::Fault);
            }
        };
        (PRIMARY_ENTRY.load(Ordering::SeqCst), engine)
    } else {
        (0, crate::xdp::engine::Engine::Aot)
    };
    if entry == 0 {
        IN_FLIGHT.store(0, Ordering::Release);
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_inactive_skip();
        return Err(RxVerdict::Fault);
    }
    // Timed here, around the engine call only. The perf aggregates bracket a
    // wider interval that includes the counter and trace hooks on both sides,
    // and those hooks cost more than a small program does, so a figure taken
    // between them describes the hooks rather than the work.
    #[cfg(feature = "perf-aggregate")]
    let t0 = crate::ext::perf::aggregate::AggregateSet::now_ticks();
    let action = unsafe { super::engine::flight(engine, entry, ctx_ptr) };
    #[cfg(feature = "perf-aggregate")]
    {
        let engine_us = crate::ext::perf::aggregate::AggregateSet::now_ticks().wrapping_sub(t0);
        crate::ext::perf::aggregate::record_engine(engine_us);
    }
    IN_FLIGHT.store(0, Ordering::Release);
    let action = action.ok_or(RxVerdict::Fault)?;

    let data = ctx_md.data;
    let Some(len) = (ctx_md.data_end as usize).checked_sub(data as usize) else {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_bad_md();
        return Err(RxVerdict::Fault);
    };
    Ok(RunResult {
        action,
        verdict: RxVerdict::Policy,
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
pub(crate) unsafe fn inner_xdp(entry: usize, ctx: *mut ctx::XdpMd) -> Option<i32> {
    stack::paint_canary();

    let sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
    }
    if sp & 15 != 0 {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_stack_misalign();
        return None;
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
    Some(result)
}
