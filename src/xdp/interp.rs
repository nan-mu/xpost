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
//! - [`MAP_VALUES`]: value windows returned by this flight's lookup calls.
//!   Packet, md and stack windows are decided by `rbpf` itself.
//! - `BUDGET_LEFT`: instructions remaining in this flight. Upstream has no
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
#[unsafe(export_name = "xpost_rbpf_stack")]
static mut EBPF_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

/// One value returned by an in-flight lookup.
#[derive(Clone, Copy)]
struct MapValue {
    map: crate::map::MapIdx,
    start: usize,
    end: usize,
}

// The ingress flight owns this table. Reset it before executing bytecode.
// Reserve one window per saved stack word and per eBPF register.
static mut MAP_VALUES: heapless::Vec<MapValue, { STACK_SIZE / 8 + 11 }> = heapless::Vec::new();

pub(crate) fn remember_map_value(map: crate::map::MapIdx, start: usize, size: usize) {
    let values = unsafe { &mut *addr_of_mut!(MAP_VALUES) };
    if values
        .iter()
        .any(|value| value.start == start && value.map == map)
    {
        return;
    }
    let _ = values.push(MapValue {
        map,
        start,
        end: start + size,
    });
}

/// Drop windows that may move or disappear during a hash map mutation.
pub(crate) fn forget_map_values(map: crate::map::MapIdx) {
    unsafe { &mut *addr_of_mut!(MAP_VALUES) }.retain(|value| value.map != map);
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
    // The program was verified when it was staged, so this does not rescan it.
    // Building the VM itself is a few word copies: `new_verified` stores the
    // program slice and one constant stack size.
    let vm = unsafe { rbpf::EbpfVmMbuff::new_verified(prog) };
    unsafe { run_with(&vm, ctx) }
}

/// Run with an already-built VM.
///
/// Split out so a measurement can hoist the construction out of a loop and see
/// how much of a flight it costs.
///
/// # Safety
///
/// Same contract as [`run`].
#[inline]
pub(crate) unsafe fn run_with(vm: &rbpf::EbpfVmMbuff<'_>, ctx: *mut XdpMd) -> i32 {
    const ABORTED: i32 = xpost_type::XdpAction::Aborted as i32;

    let prog = match vm.program() {
        Some(p) if !p.is_empty() => p,
        _ => return ABORTED,
    };

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
        core::slice::from_raw_parts(
            &md as *const EbpfXdpMd as *const u8,
            core::mem::size_of::<EbpfXdpMd>(),
        )
    };

    let stack = unsafe { &mut *addr_of_mut!(EBPF_STACK) };
    // The VM reads `r10` before writing it only inside a local-function call,
    // which an XDP program does not use, so clearing the whole 512 bytes on
    // every flight is work paid for nothing. Set `XPOST_INTERP_STACK_CLEAR=1` in
    // Cargo features to restore it for a comparison.
    #[cfg(feature = "interp-stack-clear")]
    for b in stack.iter_mut() {
        *b = 0;
    }
    // Reset the per-flight counters. Plain stores: the `IN_FLIGHT` owner is
    // the only context that touches them.
    unsafe {
        *core::ptr::addr_of_mut!(BUDGET_LEFT) = MAX_STEPS;
        (&mut *addr_of_mut!(MAP_VALUES)).clear();
    }

    let env = McuEnv::new(call_helper, check_access, step);
    match vm.execute_program_mcu(pkt, md_bytes, stack, &env) {
        Ok(r0) => r0 as u32 as i32,
        Err(_) => ABORTED,
    }
}

/// Verify a program before it is published.
///
/// Runs once per image at load time. Without it the interpreter executes
/// whatever bytes the slot holds, and a program that jumps outside its own
/// bytes reaches `ebpf::get_insn`, which panics.
pub(crate) fn verify(prog: &[u8]) -> bool {
    rbpf::EbpfVmMbuff::verify(prog).is_ok()
}

/// Why a map reference could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapPatchError {
    /// The program is not a whole number of instructions.
    BadLength,
    /// An `lddw` claimed a map index the image does not declare.
    IndexOutOfRange,
    /// A program that references maps was loaded without a binding table.
    NoBinds,
}

/// Replace the map index in every `lddw` with the map's linked address.
///
/// The bytecode says `lddw r1, <index>` with the pseudocode source bit set; the
/// index identifies which map in the image's own declaration order. The
/// interpreter has no map table, so the helper hook needs a plain address, and
/// the address the AOT code would have loaded is the one `bind` carries.
///
/// # What this does not do
///
/// It does not decide whether the program may touch that map. The lease already
/// installs the value windows those maps live in, and `check_access` refuses any
/// address outside them. This rewrites an index into an address; the permission
/// decision stays with the lease.
///
/// `binds` is the image's `aot_binds` in declaration order, which is the order
/// the packer numbered the maps in.
pub(crate) fn apply_map_addresses(prog: &mut [u8], binds: &[usize]) -> Result<(), MapPatchError> {
    use rbpf::ebpf;
    if !prog.len().is_multiple_of(ebpf::INSN_SIZE) {
        return Err(MapPatchError::BadLength);
    }
    let mut i = 0usize;
    while i < prog.len() / ebpf::INSN_SIZE {
        let at = i * ebpf::INSN_SIZE;
        let opc = prog[at];
        // `lddw` is a two-instruction form, so the second half is skipped below.
        if opc == ebpf::LD_DW_IMM {
            let src = (prog[at + 1] >> 4) & 0x0f;
            // src == 1 marks a map index rather than a constant.
            if src == 1 {
                let idx =
                    u32::from_le_bytes([prog[at + 4], prog[at + 5], prog[at + 6], prog[at + 7]])
                        as usize;
                if binds.is_empty() {
                    return Err(MapPatchError::NoBinds);
                }
                let addr = *binds.get(idx).ok_or(MapPatchError::IndexOutOfRange)?;
                if addr == 0 {
                    return Err(MapPatchError::NoBinds);
                }
                // Clear the high half of the 32-bit target address.
                prog[at + 4..at + 8].copy_from_slice(&(addr as u32).to_le_bytes());
                prog[at + 12..at + 16].copy_from_slice(&0u32.to_le_bytes());
                // Clear the source bit: the value is now an ordinary constant.
                prog[at + 1] &= 0x0f;
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    Ok(())
}

/// Verify cold bytecode and resolve its map indexes in the slot copy.
pub(crate) fn prepare_slot(prog: &mut [u8], binds: &[usize]) -> bool {
    verify(prog) && !rejects_ctx_writes(prog) && apply_map_addresses(prog, binds).is_ok()
}

/// Whether one instruction stores through `r1` while `r1` is still the context.
///
/// An XDP program must treat its context as read-only: the kernel's verifier
/// enforces that, and `rbpf` does not. A store through `r1` would let the
/// program rewrite `data`/`data_end`, which are the pointers this module trusts
/// when it builds the packet slice.
///
/// Only a store that reaches the context counts. A program is free to rebind
/// `r1` to a data pointer and store through it, which is what the compiler emits
/// for `*(u8 *)(r1 + n) = v` after `r1 += offset`; treating every `r1` store as a
/// context write rejected those programs outright. `r1_is_ctx` tracks whether the
/// register still holds the context so far.
pub(crate) fn rejects_ctx_write_insn(insn: &[u8], r1_is_ctx: bool) -> bool {
    use rbpf::ebpf;
    if insn.len() < ebpf::INSN_SIZE || !r1_is_ctx {
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

/// Whether an instruction rebinds `r1` away from the context.
///
/// True for any instruction that writes `r1` other than a store: a move, a load,
/// an arithmetic update, or a call (whose `r1` is an argument, and whose return
/// lands in `r0`). Jumps do not write their destination, so they are excluded.
fn rebinds_r1(insn: &[u8]) -> bool {
    use rbpf::ebpf;
    if insn.len() < ebpf::INSN_SIZE {
        return false;
    }
    let opc = insn[0];
    let dst = insn[1] & 0x0f;
    if dst != 1 {
        return false;
    }
    let is_store = matches!(
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
    );
    let is_jump = matches!(opc & 0x07, 0x05 | 0x06);
    !is_store && !is_jump
}

/// [`rejects_ctx_write_insn`] over a whole program, tracking `r1`'s meaning.
pub(crate) fn rejects_ctx_writes(prog: &[u8]) -> bool {
    use rbpf::ebpf;
    if !prog.len().is_multiple_of(ebpf::INSN_SIZE) {
        return false;
    }
    let mut r1_is_ctx = true;
    for i in 0..prog.len() / ebpf::INSN_SIZE {
        let insn = &prog[i * ebpf::INSN_SIZE..];
        if rejects_ctx_write_insn(insn, r1_is_ctx) {
            return true;
        }
        if rebinds_r1(insn) {
            r1_is_ctx = false;
        }
    }
    false
}

/// [`rejects_ctx_write_insn`] over a whole program.

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
    let values = unsafe { &*addr_of_mut!(MAP_VALUES) };
    values
        .iter()
        .any(|value| value.start <= addr && end <= value.end)
}

/// Charge one instruction to the flight budget.
///
/// Both counters are plain static muts touched only from the `IN_FLIGHT` owner,
/// so they need no atomics. The earlier version used `fetch_add`, which on
/// RV32IMC compiles to an interrupt-disabling sequence: two of those per
/// instruction dominated the very thing being measured.
#[inline(always)]
fn step() -> Step {
    unsafe {
        let budget = core::ptr::addr_of_mut!(BUDGET_LEFT);
        if *budget == 0 {
            return Step::Exhausted;
        }
        *budget -= 1;
        let executed = core::ptr::addr_of_mut!(EXECUTED);
        *executed = executed.read_volatile().wrapping_add(1);
    }
    Step::Go
}

/// Instructions left in the current flight. Safe as a plain static: only the
/// `IN_FLIGHT` owner touches it, and that is at most one context at a time.
static mut BUDGET_LEFT: u32 = 0;

/// Instructions executed since the last [`take_steps`].
static mut EXECUTED: u32 = 0;

/// Read and clear the executed-instruction counter.
pub fn take_steps() -> u32 {
    unsafe {
        let p = core::ptr::addr_of_mut!(EXECUTED);
        let v = p.read_volatile();
        p.write_volatile(0);
        v
    }
}

/// Instructions executed in the last flight.
///
/// A measurement aid: dividing a flight's time by this gives the cost of one
/// interpreted instruction, which is what tells apart a slow engine from a slow
/// clock.
static LAST_STEPS: AtomicU32 = AtomicU32::new(0);

/// Read the instruction count of the most recent flight.
pub fn last_steps() -> u32 {
    LAST_STEPS.load(Ordering::Relaxed)
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
    // No stage ran for this program, so verify here. The leased path verifies
    // once at load; a self-check has no loader.
    if !verify(prog) {
        return xpost_type::XdpAction::Aborted as i32;
    }
    let mut ctx = XdpMd::from_buf(frame);
    unsafe { run(prog.as_ptr() as usize, prog.len(), &mut ctx as *mut XdpMd) }
}
