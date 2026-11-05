// SPDX-License-Identifier: (Apache-2.0 OR MIT)
// Derived from uBPF <https://github.com/iovisor/ubpf>
// Copyright 2015 Big Switch Networks, Inc
//      (uBPF: VM architecture, parts of the interpreter, originally in C)
// Copyright 2016 6WIND S.A. <quentin.monnet@6wind.com>
//      (Translation to Rust, MetaBuff/multiple classes addition, hashmaps for helpers)

// Local fork: the interpreter body is generic over [`Host`] so the upstream
// `std` behaviour and the `mcu` embedding ABI share one copy of the decode
// match. `StdHost` reproduces upstream exactly; `McuHost` routes helper calls,
// memory permission and the step budget to `mcu::McuEnv`.

use crate::ebpf;
use crate::ebpf::MAX_CALL_DEPTH;
#[cfg(feature = "mcu")]
use crate::mcu::{McuEnv, Step};
use crate::lib::*;
use crate::stack::{StackFrame, StackUsage};
use core::ops::Range;
// RV32IMC has no A extension, so `target_has_atomic = "32"` is unset there.
// The `mcu` build takes `portable-atomic` instead; every other build keeps the
// upstream `core` type, which is why both arms are mutually exclusive.
#[cfg(all(not(target_has_atomic = "32"), feature = "mcu"))]
use portable_atomic::AtomicU32;
#[cfg(any(target_has_atomic = "32", not(feature = "mcu")))]
use core::sync::atomic::AtomicU32;
#[cfg(target_has_atomic = "64")]
use core::sync::atomic::AtomicU64;
use core::sync::atomic::Ordering;

/// Services the interpreter needs from its embedder.
///
/// Every method takes `&self`. Both implementations hold only shared data, so
/// the decode loop can keep one shared borrow alive for its whole body.
trait Host {
    /// Resolve helper `id` for `call` with `src == 0`.
    ///
    /// `None` is a hard error, matching upstream's "unknown helper" rejection.
    /// Returning a value here rather than a function pointer is what lets the
    /// MCU path reach a hook that carries state.
    fn call(&self, id: u32, args: [u64; 5]) -> Option<u64>;

    /// Approve one access to memory outside `mbuff`, `mem` and the stack.
    ///
    /// The whole `addr..addr + len` window must be covered.
    fn allow(&self, addr: u64, len: usize) -> bool;

    /// Called once per dispatched instruction. `false` stops the run.
    fn step(&self) -> bool;
}

/// Upstream behaviour: `HashMap` helpers, `HashSet` permitted ranges, no budget.
struct StdHost<'a> {
    helpers: &'a HashMap<u32, ebpf::Helper>,
    allowed_memory: &'a HashSet<Range<u64>>,
}

impl Host for StdHost<'_> {
    fn call(&self, id: u32, args: [u64; 5]) -> Option<u64> {
        let f = self.helpers.get(&id)?;
        Some(f(args[0], args[1], args[2], args[3], args[4]))
    }

    fn allow(&self, addr: u64, len: usize) -> bool {
        // Full-window check. Upstream tested `contains(&addr)`, which admitted
        // a window that started inside a permitted range and ended outside it.
        let end = match addr.checked_add(len as u64) {
            Some(end) => end,
            None => return false,
        };
        self.allowed_memory
            .iter()
            .any(|range| range.start <= addr && end <= range.end)
    }

    fn step(&self) -> bool {
        true
    }
}

/// MCU behaviour: all authority outside the machine comes from [`McuEnv`].
#[cfg(feature = "mcu")]
struct McuHost<'a> {
    env: &'a McuEnv,
}

#[cfg(feature = "mcu")]
impl Host for McuHost<'_> {
    fn call(&self, id: u32, args: [u64; 5]) -> Option<u64> {
        Some((self.env.call_helper)(id, args))
    }

    fn allow(&self, addr: u64, len: usize) -> bool {
        (self.env.check_access)(addr, len)
    }

    fn step(&self) -> bool {
        (self.env.step)() == Step::Go
    }
}

#[allow(clippy::too_many_arguments)]
fn check_mem(
    addr: u64,
    len: usize,
    access_type: &str,
    insn_ptr: usize,
    mbuff: &[u8],
    mem: &[u8],
    stack_ptr: usize,
    stack_len: usize,
    host: &impl Host,
) -> Result<(), Error> {
    if let Some(addr_end) = addr.checked_add(len as u64) {
        if mbuff.as_ptr() as u64 <= addr && addr_end <= mbuff.as_ptr() as u64 + mbuff.len() as u64 {
            return Ok(());
        }
        if mem.as_ptr() as u64 <= addr && addr_end <= mem.as_ptr() as u64 + mem.len() as u64 {
            return Ok(());
        }
        if stack_ptr as u64 <= addr && addr_end <= stack_ptr as u64 + stack_len as u64 {
            return Ok(());
        }
        if host.allow(addr, len) {
            return Ok(());
        }
    }

    Err(Error::other(format!(
        "Error: out of bounds memory {} (insn #{:?}), addr {:#x}, size {:?}\nmbuff: {:#x}/{:#x}, mem: {:#x}/{:#x}, stack: {:#x}/{:#x}",
        access_type, insn_ptr, addr, len,
        mbuff.as_ptr() as u64, mbuff.len(),
        mem.as_ptr() as u64, mem.len(),
        stack_ptr, stack_len
    )))
}

pub fn execute_program(
    prog_: Option<&[u8]>,
    stack_usage: Option<&StackUsage>,
    mem: &[u8],
    mbuff: &[u8],
    helpers: &HashMap<u32, ebpf::Helper>,
    allowed_memory: &HashSet<Range<u64>>,
) -> Result<u64, Error> {
    let (prog, stack_usage) = match prog_ {
        Some(prog) => (prog, stack_usage.unwrap()),
        None => Err(Error::other(
            "Error: No program set, call prog_set() to load one",
        ))?,
    };
    let mut stack = vec![0u8; ebpf::STACK_SIZE];
    let host = StdHost {
        helpers,
        allowed_memory,
    };
    execute_inner(prog, stack_usage, mem, mbuff, &mut stack, &host)
}

/// Run one program on caller-provided stack memory with no heap use (`mcu`).
///
/// Division of authority against [`execute_program`]:
///
/// | Concern | `execute_program` | `execute_program_mcu` |
/// | --- | --- | --- |
/// | eBPF stack | `Vec` per run | the caller's `stack` |
/// | helper table | `HashMap<u32, Helper>` | [`McuEnv::call_helper`] |
/// | permitted memory | `HashSet<Range<u64>>` | [`McuEnv::check_access`] |
/// | termination | the program's own `exit` | [`McuEnv::step`] budget |
///
/// `mem` and `mbuff` stay range-checked here, because their bounds are known to
/// the caller and need no embedding decision. Everything else is the
/// environment's call.
///
/// The decode loop allocates nothing. An [`Error`] does allocate, because its
/// message is a formatted `String`; that is the path a refused program takes
/// once, not the path a serviced packet takes.
///
/// # Safety
///
/// The program writes its `r10` region through raw pointers. `stack` must be at
/// least [`ebpf::STACK_SIZE`] bytes; a shorter slice is rejected before the
/// first instruction. The caller must not alias `stack` while this runs.
#[cfg(feature = "mcu")]
pub fn execute_program_mcu(
    prog: &[u8],
    stack_usage: Option<&StackUsage>,
    mem: &[u8],
    mbuff: &[u8],
    stack: &mut [u8],
    env: &McuEnv,
) -> Result<u64, Error> {
    if stack.len() < ebpf::STACK_SIZE {
        return Err(Error::other(format!(
            "Error: MCU stack too small ({} < {})",
            stack.len(),
            ebpf::STACK_SIZE
        )));
    }
    let usage = match stack_usage {
        Some(u) => u,
        None => Err(Error::other(
            "Error: no stack usage; call a constructor or a validator first",
        ))?,
    };
    for b in stack.iter_mut() {
        *b = 0;
    }
    let host = McuHost { env };
    execute_inner(prog, usage, mem, mbuff, stack, &host)
}

fn execute_inner<H: Host>(
    prog: &[u8],
    stack_usage: &StackUsage,
    mem: &[u8],
    mbuff: &[u8],
    stack: &mut [u8],
    host: &H,
) -> Result<u64, Error> {
    const U32MAX: u64 = u32::MAX as u64;
    const SHIFT_MASK_64: u64 = 0x3f;

    let stack_ptr = stack.as_mut_ptr() as usize;
    let stack_len = stack.len();
    let mut stacks = [StackFrame::new(); MAX_CALL_DEPTH];
    let mut stack_frame_idx = 0;

    // R1 points to beginning of memory area, R10 to stack
    let mut reg: [u64; 11] = [
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        stack_ptr as u64 + stack_len as u64,
    ];
    if !mbuff.is_empty() {
        reg[1] = mbuff.as_ptr() as u64;
    } else if !mem.is_empty() {
        reg[1] = mem.as_ptr() as u64;
    }

    let check_mem_load = |addr: u64, len: usize, insn_ptr: usize| {
        check_mem(
            addr,
            len,
            "load",
            insn_ptr,
            mbuff,
            mem,
            stack_ptr,
            stack_len,
            host,
        )
    };
    let check_mem_store = |addr: u64, len: usize, insn_ptr: usize| {
        check_mem(
            addr,
            len,
            "store",
            insn_ptr,
            mbuff,
            mem,
            stack_ptr,
            stack_len,
            host,
        )
    };

    // Loop on instructions
    let mut insn_ptr: usize = 0;
    while insn_ptr * ebpf::INSN_SIZE < prog.len() {
        if !host.step() {
            return Err(Error::other(format!(
                "Error: instruction budget exhausted (insn #{insn_ptr})"
            )));
        }
        let insn = ebpf::get_insn(prog, insn_ptr);
        if stack_frame_idx < MAX_CALL_DEPTH
            && let Some(usage) = stack_usage.stack_usage_for_local_func(insn_ptr) {
                stacks[stack_frame_idx].set_stack_usage(usage);
            }
        insn_ptr += 1;
        let _dst = insn.dst as usize;
        let _src = insn.src as usize;

        let mut do_jump = || {
            insn_ptr = (insn_ptr as i16 + insn.off) as usize;
        };

        macro_rules! unsigned_u64 {
            ($imm:expr) => {
                ($imm as u32) as u64
            };
        }

        #[rustfmt::skip]
        #[allow(clippy::let_unit_value)] // assign, to avoid #[rustfmt::skip] on an expression
        let _ = match insn.opc {

            // BPF_LD class
            // LD_ABS_* and LD_IND_* are supposed to load pointer to data from metadata buffer.
            // Since this pointer is constant, and since we already know it (mem), do not
            // bother re-fetching it, just use mem already.
            ebpf::LD_ABS_B   => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + (insn.imm as u32) as u64) as *const u8;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_ABS_H   => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + (insn.imm as u32) as u64) as *const u16;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_ABS_W   => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + (insn.imm as u32) as u64) as *const u32;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_ABS_DW  => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + (insn.imm as u32) as u64) as *const u64;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned()
            },
            ebpf::LD_IND_B   => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + reg[_src] + (insn.imm as u32) as u64) as *const u8;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_IND_H   => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + reg[_src] + (insn.imm as u32) as u64) as *const u16;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_IND_W   => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + reg[_src] + (insn.imm as u32) as u64) as *const u32;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_IND_DW  => reg[0] = unsafe {
                let x = (mem.as_ptr() as u64 + reg[_src] + (insn.imm as u32) as u64) as *const u64;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned()
            },

            ebpf::LD_DW_IMM  => {
                let next_insn = ebpf::get_insn(prog, insn_ptr);
                insn_ptr += 1;
                reg[_dst] = ((insn.imm as u32) as u64) + ((next_insn.imm as u64) << 32);
            },

            // BPF_LDX class
            ebpf::LD_B_REG   => reg[_dst] = unsafe {
                let x = (reg[_src] as *const u8).wrapping_offset(insn.off as isize);
                check_mem_load(x as u64, 1, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_H_REG   => reg[_dst] = unsafe {
                let x = (reg[_src] as *const u8).wrapping_offset(insn.off as isize) as *const u16;
                check_mem_load(x as u64, 2, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_W_REG   => reg[_dst] = unsafe {
                let x = (reg[_src] as *const u8).wrapping_offset(insn.off as isize) as *const u32;
                check_mem_load(x as u64, 4, insn_ptr)?;
                x.read_unaligned() as u64
            },
            ebpf::LD_DW_REG  => reg[_dst] = unsafe {
                let x = (reg[_src] as *const u8).wrapping_offset(insn.off as isize) as *const u64;
                check_mem_load(x as u64, 8, insn_ptr)?;
                x.read_unaligned()
            },

            // BPF_ST class
            ebpf::ST_B_IMM   => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u8;
                check_mem_store(x as u64, 1, insn_ptr)?;
                x.write_unaligned(insn.imm as u8);
            },
            ebpf::ST_H_IMM   => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u16;
                check_mem_store(x as u64, 2, insn_ptr)?;
                x.write_unaligned(insn.imm as u16);
            },
            ebpf::ST_W_IMM   => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u32;
                check_mem_store(x as u64, 4, insn_ptr)?;
                x.write_unaligned(insn.imm as u32);
            },
            ebpf::ST_DW_IMM  => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u64;
                check_mem_store(x as u64, 8, insn_ptr)?;
                x.write_unaligned(insn.imm as u64);
            },

            // BPF_STX class
            ebpf::ST_B_REG   => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u8;
                check_mem_store(x as u64, 1, insn_ptr)?;
                x.write_unaligned(reg[_src] as u8);
            },
            ebpf::ST_H_REG   => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u16;
                check_mem_store(x as u64, 2, insn_ptr)?;
                x.write_unaligned(reg[_src] as u16);
            },
            ebpf::ST_W_REG   => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u32;
                check_mem_store(x as u64, 4, insn_ptr)?;
                x.write_unaligned(reg[_src] as u32);
            },
            ebpf::ST_DW_REG  => unsafe {
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u64;
                check_mem_store(x as u64, 8, insn_ptr)?;
                x.write_unaligned(reg[_src]);
            },
            ebpf::ST_W_XADD  => unsafe {
                // Semantics: *(u32 *)(dst + off) += (u32)src
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u32;
                check_mem_store(x as u64, 4, insn_ptr)?;
                let add = reg[_src] as u32;

                let addr = x as usize;
                if addr.is_multiple_of(core::mem::align_of::<u32>()) {
                    // Safety: AtomicU32 has the same size/alignment as u32, and we enforce natural
                    // alignment. The pointed memory is treated as atomic for the duration of the VM
                    // execution.
                    let a = x as *const AtomicU32;
                    let _prev = (*a).fetch_add(add, Ordering::Relaxed);
                } else {
                    Err(Error::other(format!(
                        "Error: unaligned atomic XADD (ST_W_XADD) (insn #{}), addr {:#x}, required alignment {}",
                        insn_ptr - 1,
                        x as u64,
                        core::mem::align_of::<u32>()
                    )))?;
                }
            },
            #[cfg(target_has_atomic = "64")]
            ebpf::ST_DW_XADD => unsafe {
                // Semantics: *(u64 *)(dst + off) += src
                let x = (reg[_dst] as *const u8).wrapping_offset(insn.off as isize) as *mut u64;
                check_mem_store(x as u64, 8, insn_ptr)?;
                let add = reg[_src];

                let addr = x as usize;
                if addr.is_multiple_of(core::mem::align_of::<u64>()) {
                    // Safety: AtomicU64 has the same size/alignment as u64 on targets that provide
                    // 64-bit atomics, and we enforce natural alignment.
                    let a = x as *const AtomicU64;
                    let _prev = (*a).fetch_add(add, Ordering::Relaxed);
                } else {
                    Err(Error::other(format!(
                        "Error: unaligned atomic XADD (ST_DW_XADD) (insn #{}), addr {:#x}, required alignment {}",
                        insn_ptr - 1,
                        x as u64,
                        core::mem::align_of::<u64>()
                    )))?;
                }
            },
            #[cfg(not(target_has_atomic = "64"))]
            ebpf::ST_DW_XADD => Err(Error::other(format!(
                "Error: atomic XADD (ST_DW_XADD) requires 64-bit atomics on this target (insn #{})",
                insn_ptr - 1
            )))?,

            // BPF_ALU class
            // TODO Check how overflow works in kernel. Should we &= U32MAX all src register value
            // before we do the operation?
            // Cf ((0x11 << 32) - (0x1 << 32)) as u32 VS ((0x11 << 32) as u32 - (0x1 << 32) as u32
            ebpf::ADD32_IMM  => reg[_dst] = (reg[_dst] as i32).wrapping_add(insn.imm)         as u32 as u64,
            ebpf::ADD32_REG  => reg[_dst] = (reg[_dst] as i32).wrapping_add(reg[_src] as i32) as u32 as u64,
            ebpf::SUB32_IMM  => reg[_dst] = (reg[_dst] as i32).wrapping_sub(insn.imm)         as u32 as u64,
            ebpf::SUB32_REG  => reg[_dst] = (reg[_dst] as i32).wrapping_sub(reg[_src] as i32) as u32 as u64,
            ebpf::MUL32_IMM  => reg[_dst] = (reg[_dst] as i32).wrapping_mul(insn.imm)         as u32 as u64,
            ebpf::MUL32_REG  => reg[_dst] = (reg[_dst] as i32).wrapping_mul(reg[_src] as i32) as u32 as u64,
            ebpf::DIV32_IMM if insn.imm as u32 == 0 => reg[_dst] = 0,
            ebpf::DIV32_IMM  => reg[_dst] = (reg[_dst] as u32 / insn.imm              as u32) as u64,
            ebpf::DIV32_REG if reg[_src] as u32 == 0 => reg[_dst] = 0,
            ebpf::DIV32_REG  => reg[_dst] = (reg[_dst] as u32 / reg[_src]             as u32) as u64,
            ebpf::OR32_IMM   =>   reg[_dst] = (reg[_dst] as u32             | insn.imm  as u32) as u64,
            ebpf::OR32_REG   =>   reg[_dst] = (reg[_dst] as u32             | reg[_src] as u32) as u64,
            ebpf::AND32_IMM  =>   reg[_dst] = (reg[_dst] as u32             & insn.imm  as u32) as u64,
            ebpf::AND32_REG  =>   reg[_dst] = (reg[_dst] as u32             & reg[_src] as u32) as u64,
            // As for the 64-bit version, we should mask the number of bits to shift with
            // 0x1f, but .wrappping_shr() already takes care of it for us.
            ebpf::LSH32_IMM  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shl(insn.imm  as u32) as u64,
            ebpf::LSH32_REG  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shl(reg[_src] as u32) as u64,
            ebpf::RSH32_IMM  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shr(insn.imm  as u32) as u64,
            ebpf::RSH32_REG  =>   reg[_dst] = (reg[_dst] as u32).wrapping_shr(reg[_src] as u32) as u64,
            ebpf::NEG32      => { reg[_dst] = (reg[_dst] as i32).wrapping_neg()                 as u64; reg[_dst] &= U32MAX; },
            ebpf::MOD32_IMM if insn.imm as u32 == 0 => (),
            ebpf::MOD32_IMM  =>   reg[_dst] = (reg[_dst] as u32             % insn.imm  as u32) as u64,
            ebpf::MOD32_REG if reg[_src] as u32 == 0 => (),
            ebpf::MOD32_REG  =>   reg[_dst] = (reg[_dst] as u32 % reg[_src]             as u32) as u64,
            ebpf::XOR32_IMM  =>   reg[_dst] = (reg[_dst] as u32             ^ insn.imm  as u32) as u64,
            ebpf::XOR32_REG  =>   reg[_dst] = (reg[_dst] as u32             ^ reg[_src] as u32) as u64,
            ebpf::MOV32_IMM  =>   reg[_dst] = insn.imm   as u32                                 as u64,
            ebpf::MOV32_REG  =>   reg[_dst] = (reg[_src] as u32)                                as u64,
            // As for the 64-bit version, we should mask the number of bits to shift with
            // 0x1f, but .wrappping_shr() already takes care of it for us.
            ebpf::ARSH32_IMM => { reg[_dst] = (reg[_dst] as i32).wrapping_shr(insn.imm  as u32) as u64; reg[_dst] &= U32MAX; },
            ebpf::ARSH32_REG => { reg[_dst] = (reg[_dst] as i32).wrapping_shr(reg[_src] as u32) as u64; reg[_dst] &= U32MAX; },
            ebpf::LE         => {
                reg[_dst] = match insn.imm {
                    16 => (reg[_dst] as u16).to_le() as u64,
                    32 => (reg[_dst] as u32).to_le() as u64,
                    64 =>  reg[_dst].to_le(),
                    _  => unreachable!(),
                };
            },
            ebpf::BE         => {
                reg[_dst] = match insn.imm {
                    16 => (reg[_dst] as u16).to_be() as u64,
                    32 => (reg[_dst] as u32).to_be() as u64,
                    64 =>  reg[_dst].to_be(),
                    _  => unreachable!(),
                };
            },

            // BPF_ALU64 class
            ebpf::ADD64_IMM  => reg[_dst] = reg[_dst].wrapping_add(insn.imm as u64),
            ebpf::ADD64_REG  => reg[_dst] = reg[_dst].wrapping_add(reg[_src]),
            ebpf::SUB64_IMM  => reg[_dst] = reg[_dst].wrapping_sub(insn.imm as u64),
            ebpf::SUB64_REG  => reg[_dst] = reg[_dst].wrapping_sub(reg[_src]),
            ebpf::MUL64_IMM  => reg[_dst] = reg[_dst].wrapping_mul(insn.imm as u64),
            ebpf::MUL64_REG  => reg[_dst] = reg[_dst].wrapping_mul(reg[_src]),
            ebpf::DIV64_IMM if insn.imm == 0 => reg[_dst] = 0,
            ebpf::DIV64_IMM  => reg[_dst]                       /= insn.imm as u64,
            ebpf::DIV64_REG if reg[_src] == 0 => reg[_dst] = 0,
            ebpf::DIV64_REG  => reg[_dst] /= reg[_src],
            ebpf::OR64_IMM   => reg[_dst] |=  insn.imm as u64,
            ebpf::OR64_REG   => reg[_dst] |=  reg[_src],
            ebpf::AND64_IMM  => reg[_dst] &=  insn.imm as u64,
            ebpf::AND64_REG  => reg[_dst] &=  reg[_src],
            ebpf::LSH64_IMM  => reg[_dst] <<= insn.imm as u64 & SHIFT_MASK_64,
            ebpf::LSH64_REG  => reg[_dst] <<= reg[_src] & SHIFT_MASK_64,
            ebpf::RSH64_IMM  => reg[_dst] >>= insn.imm as u64 & SHIFT_MASK_64,
            ebpf::RSH64_REG  => reg[_dst] >>= reg[_src] & SHIFT_MASK_64,
            ebpf::NEG64      => reg[_dst] = -(reg[_dst] as i64) as u64,
            ebpf::MOD64_IMM if insn.imm == 0 => (),
            ebpf::MOD64_IMM  => reg[_dst] %=  insn.imm as u64,
            ebpf::MOD64_REG if reg[_src] == 0 => (),
            ebpf::MOD64_REG  => reg[_dst] %=  reg[_src],
            ebpf::XOR64_IMM  => reg[_dst] ^= insn.imm  as u64,
            ebpf::XOR64_REG  => reg[_dst] ^= reg[_src],
            ebpf::MOV64_IMM  => reg[_dst] =  insn.imm  as u64,
            ebpf::MOV64_REG  => reg[_dst] =  reg[_src],
            ebpf::ARSH64_IMM => reg[_dst] = (reg[_dst] as i64 >> (insn.imm as u64 & SHIFT_MASK_64))  as u64,
            ebpf::ARSH64_REG => reg[_dst] = (reg[_dst] as i64 >> (reg[_src] as u64 & SHIFT_MASK_64)) as u64,

            // BPF_JMP class
            // TODO: check this actually works as expected for signed / unsigned ops
            // J-EQ, J-NE, J-GT, J-GE, J-LT, J-LE: unsigned
            // JS-GT, JS-GE, JS-LT, JS-LE: signed
            ebpf::JA         =>                                             do_jump(),
            ebpf::JEQ_IMM    => if  reg[_dst] == unsigned_u64!(insn.imm)  { do_jump(); },
            ebpf::JEQ_REG    => if  reg[_dst] == reg[_src]                { do_jump(); },
            ebpf::JGT_IMM    => if  reg[_dst] >  unsigned_u64!(insn.imm)  { do_jump(); },
            ebpf::JGT_REG    => if  reg[_dst] >  reg[_src]                { do_jump(); },
            ebpf::JGE_IMM    => if  reg[_dst] >= unsigned_u64!(insn.imm)  { do_jump(); },
            ebpf::JGE_REG    => if  reg[_dst] >= reg[_src]                { do_jump(); },
            ebpf::JLT_IMM    => if  reg[_dst] <  unsigned_u64!(insn.imm)  { do_jump(); },
            ebpf::JLT_REG    => if  reg[_dst] <  reg[_src]                { do_jump(); },
            ebpf::JLE_IMM    => if  reg[_dst] <= unsigned_u64!(insn.imm)  { do_jump(); },
            ebpf::JLE_REG    => if  reg[_dst] <= reg[_src]                { do_jump(); },
            ebpf::JSET_IMM   => if  reg[_dst] &  insn.imm as u64 != 0     { do_jump(); },
            ebpf::JSET_REG   => if  reg[_dst] &  reg[_src]       != 0     { do_jump(); },
            ebpf::JNE_IMM    => if  reg[_dst] != unsigned_u64!(insn.imm)  { do_jump(); },
            ebpf::JNE_REG    => if  reg[_dst] != reg[_src]                { do_jump(); },
            ebpf::JSGT_IMM   => if  reg[_dst] as i64  >  insn.imm  as i64 { do_jump(); },
            ebpf::JSGT_REG   => if  reg[_dst] as i64  >  reg[_src] as i64 { do_jump(); },
            ebpf::JSGE_IMM   => if  reg[_dst] as i64  >= insn.imm  as i64 { do_jump(); },
            ebpf::JSGE_REG   => if  reg[_dst] as i64  >= reg[_src] as i64 { do_jump(); },
            ebpf::JSLT_IMM   => if (reg[_dst] as i64) <  insn.imm  as i64 { do_jump(); },
            ebpf::JSLT_REG   => if (reg[_dst] as i64) <  reg[_src] as i64 { do_jump(); },
            ebpf::JSLE_IMM   => if  reg[_dst] as i64  <= insn.imm  as i64 { do_jump(); },
            ebpf::JSLE_REG   => if  reg[_dst] as i64  <= reg[_src] as i64 { do_jump(); },

            // BPF_JMP32 class
            ebpf::JEQ_IMM32  => if  reg[_dst] as u32  == insn.imm  as u32      { do_jump(); },
            ebpf::JEQ_REG32  => if  reg[_dst] as u32  == reg[_src] as u32      { do_jump(); },
            ebpf::JGT_IMM32  => if  reg[_dst] as u32  >  insn.imm  as u32      { do_jump(); },
            ebpf::JGT_REG32  => if  reg[_dst] as u32  >  reg[_src] as u32      { do_jump(); },
            ebpf::JGE_IMM32  => if  reg[_dst] as u32  >= insn.imm  as u32      { do_jump(); },
            ebpf::JGE_REG32  => if  reg[_dst] as u32  >= reg[_src] as u32      { do_jump(); },
            ebpf::JLT_IMM32  => if (reg[_dst] as u32) <  insn.imm  as u32      { do_jump(); },
            ebpf::JLT_REG32  => if (reg[_dst] as u32) <  reg[_src] as u32      { do_jump(); },
            ebpf::JLE_IMM32  => if  reg[_dst] as u32  <= insn.imm  as u32      { do_jump(); },
            ebpf::JLE_REG32  => if  reg[_dst] as u32  <= reg[_src] as u32      { do_jump(); },
            ebpf::JSET_IMM32 => if  reg[_dst] as u32  &  insn.imm  as u32 != 0 { do_jump(); },
            ebpf::JSET_REG32 => if  reg[_dst] as u32  &  reg[_src] as u32 != 0 { do_jump(); },
            ebpf::JNE_IMM32  => if  reg[_dst] as u32  != insn.imm  as u32      { do_jump(); },
            ebpf::JNE_REG32  => if  reg[_dst] as u32  != reg[_src] as u32      { do_jump(); },
            ebpf::JSGT_IMM32 => if  reg[_dst] as i32  >  insn.imm              { do_jump(); },
            ebpf::JSGT_REG32 => if  reg[_dst] as i32  >  reg[_src] as i32      { do_jump(); },
            ebpf::JSGE_IMM32 => if  reg[_dst] as i32  >= insn.imm              { do_jump(); },
            ebpf::JSGE_REG32 => if  reg[_dst] as i32  >= reg[_src] as i32      { do_jump(); },
            ebpf::JSLT_IMM32 => if (reg[_dst] as i32) <  insn.imm              { do_jump(); },
            ebpf::JSLT_REG32 => if (reg[_dst] as i32) <  reg[_src] as i32      { do_jump(); },
            ebpf::JSLE_IMM32 => if  reg[_dst] as i32  <= insn.imm              { do_jump(); },
            ebpf::JSLE_REG32 => if  reg[_dst] as i32  <= reg[_src] as i32      { do_jump(); },

            // Do not delegate the check to the verifier, since registered functions can be
            // changed after the program has been verified.
            ebpf::CALL       => {
                match _src {
                    // Call helper function
                    0 => {
                        match host.call(insn.imm as u32, [reg[1], reg[2], reg[3], reg[4], reg[5]]) {
                            Some(value) => reg[0] = value,
                            None => Err(Error::other(format!(
                                "Error: unknown helper function (id: {:#x})",
                                insn.imm as u32
                            )))?,
                        }
                    }
                    // eBPF-to-eBPF call
                    1 => {
                        if stack_frame_idx >= MAX_CALL_DEPTH {
                            Err(Error::other(
                                format!(
                                    "Error: too many nested calls (max: {MAX_CALL_DEPTH})"
                                )
                            ))?;
                        }
                        stacks[stack_frame_idx].save_registers(&reg[6..=9]);
                        stacks[stack_frame_idx].save_return_address(insn_ptr);
                        // Why we don't need to check the stack usage here?
                        // When the stack is exhausted, if there are instructions in the new function
                        // that read or write to the stack, check_mem_load or check_mem_store will return an error.
                        reg[10] -= stacks[stack_frame_idx].get_stack_usage().stack_usage() as u64;
                        stack_frame_idx += 1;
                        insn_ptr += insn.imm as usize;
                    }
                    _ => {
                        Err(Error::other(
                            format!("Error: unsupported call type #{} (insn #{})",
                                _src,
                                insn_ptr-1
                            )
                        ))?;
                    }
                }
            }
            ebpf::TAIL_CALL  => Err(Error::other(format!("Error: TAIL_CALL is not supported (insn #{})", insn_ptr - 1)))?,
            ebpf::EXIT       => {
                if stack_frame_idx > 0 {
                    stack_frame_idx -= 1;
                    reg[6..=9].copy_from_slice(&stacks[stack_frame_idx].get_registers());
                    insn_ptr = stacks[stack_frame_idx].get_return_address();
                    reg[10] += stacks[stack_frame_idx].get_stack_usage().stack_usage() as u64;
                } else {
                    return Ok(reg[0]);
                }
            }

            _                => unreachable!()
        };
    }

    unreachable!()
}
