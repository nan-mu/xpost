//! eBPF interpreter engine (`interp` feature).
//!
//! Runs the bytecode `libbpf-mcu` produced, through the local `rbpf` fork built
//! with its `mcu` feature. That build has no allocator, no JIT, and no map type;
//! everything the VM needs from the outside arrives through
//! [`rbpf::mcu::McuEnv`].
//!
//! # Why this looks the way it does
//!
//! `rbpf` treats its `mbuff` argument as R1 and the VM's normal memory. For XDP
//! that is exactly `struct xdp_md`, so the md buffer is handed over as `mbuff`.
//! Packet bytes are a separate slice, reached only through the pointers the
//! program loads out of the md. Both are range-checked by `rbpf`'s `check_mem`;
//! map values are not, and arrive through [`check_access`].
//!
//! The VM is built once per lease, not per flight: `EbpfVmMbuff::new` runs the
//! rbpf verifier, and `execute_program_mcu` does not repeat it.
//!
//! # State
//!
//! - [`EBPF_STACK`]: the program's 512-byte `r10` region. Distinct from
//!   [`super::ingress::XDP_FRAME`], which preserves the caller's registers across
//!   the `jalr` on the AOT path.
//! - [`ALLOW`]: map value windows, installed at lease and cleared at release.
//!   Packet, md and stack windows are decided by `rbpf` itself.
//! - [`BUDGET`]: instructions remaining in this flight. Upstream has no
//!   termination guard; rBPF's own paper names the missing CPU cap as its next
//!   step, and Femto-Containers monitors branches for the same reason.

use core::ptr::addr_of_mut;

use portable_atomic::{AtomicU32, Ordering};
use rbpf::ebpf::STACK_SIZE;
use rbpf::mcu::{McuEnv, Step};

use super::ingress::ctx::XdpMd;

/// Instruction ceiling for one flight.
///
/// A `fib` or `bsort` workload is well under a thousand instructions. This
/// bound is set high enough not to reject a legitimate program and low enough
/// that a runaway loop ends inside one Wi-Fi frame interval.
const MAX_STEPS: u32 = 200_000;

/// The program's `r10` region.
#[unsafe(link_section = ".bss")]
static mut EBPF_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

/// Instructions left in the current flight.
static BUDGET: AtomicU32 = AtomicU32::new(0);

/// Map value windows the program may touch: `(start, size)` pairs.
///
/// Written by [`set_ranges`] at lease, before `ACTIVE` is stored, and cleared
/// at release after `IN_FLIGHT` drains. Reads therefore never race a write.
const MAX_RANGES: usize = 16;
static mut RANGES: [(usize, usize); MAX_RANGES] = [(0, 0); MAX_RANGES];
static RANGE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Install the map value windows for the lease that is about to be published.
///
/// Each pair is `(start, len)` of one value region. A program may only load and
/// store inside these windows plus the packet, the md and its own stack.
pub(crate) fn set_ranges(ranges: &[(usize, usize)]) {
    let n = ranges.len().min(MAX_RANGES);
    let table = unsafe { &mut *addr_of_mut!(RANGES) };
    for (i, &r) in ranges[..n].iter().enumerate() {
        table[i] = r;
    }
    for slot in table.iter_mut().skip(n) {
        *slot = (0, 0);
    }
    RANGE_COUNT.store(n as u32, Ordering::Release);
}

/// Drop every window. Called after the lease is gone.
pub(crate) fn clear_ranges() {
    RANGE_COUNT.store(0, Ordering::Release);
}

/// `struct xdp_md` as eBPF bytecode sees it.
///
/// Six 32-bit fields. `data`/`data_end` hold **real addresses**, not offsets:
/// the C sources read them as `(long)ctx->data` and cast the result to a
/// pointer, and on a 32-bit target a pointer is exactly four bytes. The field
/// type is `__u32` only because `vmlinux.h` is architecture independent.
///
/// This matches [`super::ingress::ctx::XdpMd`] field for field on RV32. It is a
/// distinct type because the AOT path stores `*mut u8` and this one must not
/// carry a pointer type the interpreter would then have to launder.
#[repr(C)]
struct EbpfXdpMd {
    data: u32,
    data_end: u32,
    data_meta: u32,
    ingress_ifindex: u32,
    rx_queue_index: u32,
    egress_ifindex: u32,
}

impl EbpfXdpMd {
    /// Context over the frame the flight handed us.
    fn for_frame(data: *mut u8, data_end: *mut u8) -> Self {
        Self {
            data: data as usize as u32,
            data_end: data_end as usize as u32,
            data_meta: data as usize as u32,
            ingress_ifindex: super::ingress::ctx::IFINDEX_WIFI,
            rx_queue_index: 0,
            egress_ifindex: 0,
        }
    }
}
///
/// Returns the program's `r0` as an `xdp_action`. Every failure path returns
/// `XDP_ABORTED`, which the caller maps to `Drop`: a refused run must not look
/// like a legitimate verdict.
///
/// The packet range comes from the AOT-path context, which holds real pointers.
/// The program itself sees [`EbpfXdpMd`], the offset-based form its compiler
/// emitted against. Translating once here keeps both engines fed from the same
/// flight state.
///
/// # Safety
///
/// `base..base + len` must be the published bytecode of the current lease and
/// stay readable for the call. `ctx` must point to a live [`XdpMd`] whose
/// `data..data_end` lies inside the caller's frame, and the caller must own
/// `IN_FLIGHT`.
#[inline]
pub(crate) unsafe fn run(base: usize, len: usize, ctx: *mut XdpMd) -> i32 {
    const ABORTED: i32 = xpost_type::XdpAction::Aborted as i32;

    if len == 0 || !len.is_multiple_of(rbpf::ebpf::INSN_SIZE) {
        return ABORTED;
    }
    let prog = unsafe { core::slice::from_raw_parts(base as *const u8, len) };

    // Packet window, validated before it becomes a slice. A malformed md must
    // not produce a slice that runs off the frame.
    let (data, data_end) = unsafe { ((*ctx).data as usize, (*ctx).data_end as usize) };
    let Some(pkt_len) = data_end.checked_sub(data) else {
        return ABORTED;
    };
    if pkt_len > isize::MAX as usize {
        return ABORTED;
    }
    let pkt = unsafe { core::slice::from_raw_parts(data as *const u8, pkt_len) };

    // The program's R1. Offsets, not pointers: the compiler emitted against the
    // six-`u32` form, so a pointer here makes the program's first bound check
    // fail and it exits without touching the frame.
    let md = EbpfXdpMd::for_frame(data as *mut u8, data_end as *mut u8);
    let md_bytes = unsafe {
        core::slice::from_raw_parts(&md as *const EbpfXdpMd as *const u8, core::mem::size_of::<EbpfXdpMd>())
    };

    let stack = unsafe { &mut *addr_of_mut!(EBPF_STACK) };
    for b in stack.iter_mut() {
        *b = 0;
    }
    BUDGET.store(MAX_STEPS, Ordering::Relaxed);

    let env = McuEnv::new(call_helper, check_access, step);
    let vm = match rbpf::EbpfVmMbuff::new(Some(prog)) {
        Ok(vm) => vm,
        Err(_) => return ABORTED,
    };
    match vm.execute_program_mcu(pkt, md_bytes, stack, &env) {
        Ok(r0) => r0 as u32 as i32,
        Err(_) => ABORTED,
    }
}

/// Whether one instruction stores through `r1`.
///
/// An XDP program must treat its context as read-only: the kernel's verifier
/// enforces that, and `rbpf` does not. A store through `r1` would let the
/// program rewrite `data`/`data_end`, which are the pointers this module trusts
/// when it builds the packet slice. Reject those programs when the image is
/// leased so the flight path needs no check.
pub(crate) fn rejects_ctx_write_insn(insn: &[u8]) -> bool {
    use rbpf::ebpf;
    if insn.len() < ebpf::INSN_SIZE {
        return false;
    }
    let opc = insn[0];
    let dst = insn[1] & 0x0f;
    dst == 1
        && matches!(
            opc,
            ebpf::ST_B_IMM
                | ebpf::ST_H_IMM
                | ebpf::ST_W_IMM
                | ebpf::ST_DW_IMM
                | ebpf::ST_B_REG
                | ebpf::ST_H_REG
                | ebpf::ST_W_REG
                | ebpf::ST_DW_REG
                | ebpf::ST_W_XADD
        )
}

/// [`rejects_ctx_write_insn`] over a whole program.
pub(crate) fn rejects_ctx_writes(prog: &[u8]) -> bool {
    use rbpf::ebpf;
    if !prog.len().is_multiple_of(ebpf::INSN_SIZE) {
        return false;
    }
    (0..prog.len() / ebpf::INSN_SIZE)
        .any(|i| rejects_ctx_write_insn(&prog[i * ebpf::INSN_SIZE..]))
}

/// Helper dispatch for `call` with `src == 0`.
///
/// `rbpf` hands over `u64` registers. RV32IMC pointers and offsets fit in 32
/// bits, so the cast is lossless for everything a verified XDP program passes.
/// A program that did pass a wider value would lose the high half here: that is
/// the documented ABI, not a silent bug, and the verifier already restricts
/// what can reach a helper argument.
fn call_helper(id: u32, args: [u64; 5]) -> u64 {
    crate::helper::helper_dispatcher(
        id as usize,
        args[0] as usize,
        args[1] as usize,
        args[2] as usize,
        args[3] as usize,
        args[4] as usize,
    ) as u64
}

/// Whether one load or store may touch `addr..addr + len`.
///
/// Only map value windows reach here. `rbpf` decides the packet, the md and the
/// VM stack itself, because those bounds are known to it; a map value pointer
/// comes out of a helper and its bounds are known only to this crate.
fn check_access(addr: u64, len: usize) -> bool {
    let Ok(addr) = usize::try_from(addr) else {
        return false;
    };
    let Some(end) = addr.checked_add(len) else {
        return false;
    };
    let n = RANGE_COUNT.load(Ordering::Acquire) as usize;
    let table = unsafe { &*addr_of_mut!(RANGES) };
    table[..n]
        .iter()
        .any(|&(start, size)| start <= addr && end <= start.saturating_add(size))
}

/// Charge one instruction to the flight budget.
fn step() -> Step {
    if BUDGET.load(Ordering::Relaxed) == 0 {
        return Step::Exhausted;
    }
    BUDGET.fetch_sub(1, Ordering::Relaxed);
    Step::Go
}

/// Run one program outside a lease, for a firmware self-check.
///
/// Builds the `xdp_md` over `frame` internally, so a board example needs no
/// context type. The lease path reaches the same [`run`] through
/// [`super::engine::flight`]; this wrapper proves the interpreter works before
/// any image is installed. It installs no map windows, so a program that touches
/// a map value is refused.
///
/// Returns the program's `r0`. After the call `frame` holds whatever the
/// program wrote, which is what makes this a useful check: the caller can
/// compare the mutated bytes.
pub fn selftest_run(prog: &[u8], frame: &mut [u8]) -> i32 {
    clear_ranges();
    let mut ctx = XdpMd::from_buf(frame);
    unsafe { run(prog.as_ptr() as usize, prog.len(), &mut ctx as *mut XdpMd) }
}
