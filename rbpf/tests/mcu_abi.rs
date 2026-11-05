// SPDX-License-Identifier: (Apache-2.0 OR MIT)
//! MCU ABI hook regression tests (`mcu` feature).
//!
//! These pin the three places where an embedding runtime takes control, plus
//! the one memory-permission defect the fork repairs. Every case is expressed
//! as eBPF bytecode, because that is the only interface the hooks see.

#![cfg(feature = "mcu")]

use core::cell::Cell;

use rbpf::ebpf;
use rbpf::mcu::{McuEnv, Step};

// ── bytecode helpers ────────────────────────────────────────────────────────
//
// One `Insn` is a little-endian 8-byte eBPF instruction, which is exactly how
// `ebpf::get_insn` decodes a `&[u8]`. Building programs this way keeps the test
// independent of the assembler module.

fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0] = opc;
    out[1] = (src << 4) | (dst & 0x0f);
    out[2..4].copy_from_slice(&off.to_le_bytes());
    out[4..8].copy_from_slice(&imm.to_le_bytes());
    out
}

/// Build an `LD_DW_IMM` pair: a 64-bit immediate load is two instructions, the
/// second one a zero-opcode placeholder that carries the high word.
fn ld_dw_imm(dst: u8, value: u64) -> [[u8; 8]; 2] {
    [
        insn(ebpf::LD_DW_IMM, dst, 0, 0, value as u32 as i32),
        insn(0, 0, 0, 0, (value >> 32) as u32 as i32),
    ]
}

fn prog(parts: &[[u8; 8]]) -> std::vec::Vec<u8> {
    parts.iter().flat_map(|i| i.iter().copied()).collect()
}

const R0: u8 = 0;
const R1: u8 = 1;

// ── hook state ──────────────────────────────────────────────────────────────
//
// `McuEnv` carries bare function pointers, so a test cannot pass a closure in.
// The counters therefore live in thread-locals, which is sound because these
// tests drive one run at a time on one thread.

std::thread_local! {
    static HELPER_CALLS: Cell<u32> = const { Cell::new(0) };
    static LAST_ID: Cell<u32> = const { Cell::new(0) };
    static LAST_ARG0: Cell<u64> = const { Cell::new(0) };
    static STEPS: Cell<u32> = const { Cell::new(0) };
    static BUDGET: Cell<u32> = const { Cell::new(u32::MAX) };
    static ALLOW_BASE: Cell<u64> = const { Cell::new(0) };
    static ALLOW_LEN: Cell<u64> = const { Cell::new(0) };
    static DENY: Cell<bool> = const { Cell::new(false) };
}

fn reset() {
    HELPER_CALLS.with(|c| c.set(0));
    STEPS.with(|c| c.set(0));
    BUDGET.with(|c| c.set(u32::MAX));
    DENY.with(|c| c.set(false));
}

fn helper_hook(id: u32, args: [u64; 5]) -> u64 {
    HELPER_CALLS.with(|c| c.set(c.get() + 1));
    LAST_ID.with(|c| c.set(id));
    LAST_ARG0.with(|c| c.set(args[0]));
    // Echo a fixed sentinel so the test can tell the return value apart from
    // any register the program had before the call.
    0xabcd
}

fn access_hook(addr: u64, len: usize) -> bool {
    if DENY.with(|d| d.get()) {
        return false;
    }
    let base = ALLOW_BASE.with(|c| c.get());
    let alen = ALLOW_LEN.with(|c| c.get());
    if base == 0 {
        return false;
    }
    let end = match addr.checked_add(len as u64) {
        Some(end) => end,
        None => return false,
    };
    base <= addr && end <= base + alen
}

fn step_hook() -> Step {
    STEPS.with(|c| c.set(c.get() + 1));
    let budget = BUDGET.with(|c| c.get());
    let used = STEPS.with(|c| c.get());
    if used > budget {
        Step::Exhausted
    } else {
        Step::Go
    }
}

fn env() -> McuEnv {
    McuEnv::new(helper_hook, access_hook, step_hook)
}

// ── tests ───────────────────────────────────────────────────────────────────

#[test]
fn helper_call_goes_through_the_hook() {
    reset();
    // mov64 r0, 0 ; mov64 r1, 7 ; call 6 ; exit
    let code = prog(&[
        insn(ebpf::MOV64_IMM, R0, 0, 0, 0),
        insn(ebpf::MOV64_IMM, R1, 0, 0, 7),
        insn(ebpf::CALL, 0, 0, 0, 6),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];
    let ret = vm.execute_program_mcu(&[], &[], &mut stack, &env()).unwrap();

    assert_eq!(ret, 0xabcd, "helper return value must land in r0");
    assert_eq!(HELPER_CALLS.with(|c| c.get()), 1);
    assert_eq!(LAST_ID.with(|c| c.get()), 6, "hook must receive the imm as id");
    assert_eq!(LAST_ARG0.with(|c| c.get()), 7, "hook must receive r1..r5");
}

#[test]
fn unknown_helper_is_rejected_not_silently_zeroed() {
    reset();
    // A helper id the environment does not know is the environment's call. This
    // hook answers everything, so the run must succeed. The upstream path, in
    // contrast, returns an error for an unregistered id.
    let code = prog(&[
        insn(ebpf::MOV64_IMM, R0, 0, 0, 0),
        insn(ebpf::CALL, 0, 0, 0, 0x9999),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];
    vm.execute_program_mcu(&[], &[], &mut stack, &env()).unwrap();
    assert_eq!(LAST_ID.with(|c| c.get()), 0x9999);
}

#[test]
fn stack_stays_inside_the_caller_buffer() {
    reset();
    // Store to r10-8 then exit. `r10` is the caller's stack top.
    let code = prog(&[
        insn(ebpf::ST_DW_IMM, 10, 0, -8, 0x1234),
        insn(ebpf::MOV64_IMM, R0, 0, 0, 0),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];
    vm.execute_program_mcu(&[], &[], &mut stack, &env()).unwrap();
    // The write must land inside `stack` and nowhere else.
    let tail = &stack[stack.len() - 8..];
    assert_eq!(tail, &0x1234u64.to_le_bytes());
}

#[test]
fn a_short_stack_is_refused_before_execution() {
    reset();
    let code = prog(&[insn(ebpf::EXIT, 0, 0, 0, 0)]);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut short = [0u8; 256];
    let err = vm
        .execute_program_mcu(&[], &[], &mut short, &env())
        .unwrap_err();
    assert!(
        err.to_string().contains("stack too small"),
        "unexpected error: {err}"
    );
    assert_eq!(
        STEPS.with(|c| c.get()),
        0,
        "no instruction may run on a refused stack"
    );
}

#[test]
fn step_budget_stops_a_run_that_only_the_budget_can_end() {
    reset();
    // The upstream verifier rejects `ja -1`, so a self-loop cannot reach the
    // interpreter through the constructor. The budget is still the only guard
    // against a loop the verifier cannot see, so exercise it over a long
    // straight line instead: one `ja +0` per instruction keeps the run going
    // while the step hook counts.
    BUDGET.with(|c| c.set(5));
    let mut parts: std::vec::Vec<[u8; 8]> = std::vec::Vec::new();
    for _ in 0..40 {
        parts.push(insn(ebpf::JA, 0, 0, 0, 0));
    }
    parts.push(insn(ebpf::EXIT, 0, 0, 0, 0));
    let code = prog(&parts);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];
    let err = vm
        .execute_program_mcu(&[], &[], &mut stack, &env())
        .unwrap_err();
    assert!(
        err.to_string().contains("budget exhausted"),
        "unexpected error: {err}"
    );
    assert!(
        STEPS.with(|c| c.get()) <= 6,
        "budget must stop the run promptly, ran {}",
        STEPS.with(|c| c.get())
    );
}

#[test]
fn permitted_memory_is_the_last_resort_before_rejection() {
    reset();
    let real = [0x5au8; 64];
    let real_addr = real.as_ptr() as u64;
    let parts = {
        let mut v: std::vec::Vec<[u8; 8]> = std::vec::Vec::new();
        v.extend(ld_dw_imm(R1, real_addr));
        v.push(insn(ebpf::LD_B_REG, R0, R1, 0, 0));
        v.push(insn(ebpf::EXIT, 0, 0, 0, 0));
        v
    };
    let code = prog(&parts);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];

    // Nothing permitted: the hook is the only gate, and it refuses.
    let err = vm
        .execute_program_mcu(&[], &[], &mut stack, &env())
        .unwrap_err();
    assert!(err.to_string().contains("out of bounds"), "{err}");

    // Permit exactly the buffer, then the same program runs.
    reset();
    ALLOW_BASE.with(|c| c.set(real_addr));
    ALLOW_LEN.with(|c| c.set(real.len() as u64));
    let ret = vm.execute_program_mcu(&[], &[], &mut stack, &env()).unwrap();
    assert_eq!(ret, 0x5a);
}

#[test]
fn a_window_past_the_end_of_a_permitted_range_is_rejected() {
    reset();
    // The defect this fork repairs: upstream asked whether the *first byte* was
    // inside a permitted range. An 8-byte load whose first byte sits on the last
    // permitted byte therefore passed. The hook sees the window and refuses.
    let buf = [0u8; 16];
    let base = buf.as_ptr() as u64;
    ALLOW_BASE.with(|c| c.set(base));
    ALLOW_LEN.with(|c| c.set(buf.len() as u64));

    // Load 8 bytes starting at offset 12: 12..20 straddles the end at 16.
    let parts = {
        let mut v: std::vec::Vec<[u8; 8]> = std::vec::Vec::new();
        v.extend(ld_dw_imm(R1, base + 12));
        v.push(insn(ebpf::LD_DW_REG, R0, R1, 0, 0));
        v.push(insn(ebpf::EXIT, 0, 0, 0, 0));
        v
    };
    let code = prog(&parts);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];
    let err = vm
        .execute_program_mcu(&[], &[], &mut stack, &env())
        .unwrap_err();
    assert!(
        err.to_string().contains("out of bounds"),
        "a straddling window must be refused, got: {err}"
    );
}

#[test]
fn the_std_path_refuses_the_same_straddling_window() {
    // Same defect, upstream API. `register_allowed_memory` is the only knob, so
    // the passed range ends at the buffer end and the load straddles it.
    let buf = [0u8; 16];
    let base = buf.as_ptr() as u64;
    let parts = {
        let mut v: std::vec::Vec<[u8; 8]> = std::vec::Vec::new();
        v.extend(ld_dw_imm(R1, base + 12));
        v.push(insn(ebpf::LD_DW_REG, R0, R1, 0, 0));
        v.push(insn(ebpf::EXIT, 0, 0, 0, 0));
        v
    };
    let code = prog(&parts);
    let mut vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    vm.register_allowed_memory(base..base + buf.len() as u64);
    let err = vm.execute_program(&[], &[]).unwrap_err();
    assert!(
        err.to_string().contains("out of bounds"),
        "upstream admitted this window; the fork must not, got: {err}"
    );
}

#[test]
fn register_helper_reports_the_mcu_mismatch() {
    let code = prog(&[insn(ebpf::EXIT, 0, 0, 0, 0)]);
    let mut vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let err = vm
        .register_helper(6, rbpf::helpers::bpf_trace_printf)
        .unwrap_err();
    assert!(err.to_string().contains("McuEnv"), "{err}");
}

#[test]
fn deny_all_is_a_closed_environment() {
    reset();
    // One helper call: the environment must answer, and deny_all answers 0.
    let code = prog(&[
        insn(ebpf::MOV64_IMM, R0, 0, 0, 9),
        insn(ebpf::CALL, 0, 0, 0, 1),
        insn(ebpf::EXIT, 0, 0, 0, 0),
    ]);
    let vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let mut stack = [0u8; 512];
    let ret = vm
        .execute_program_mcu(&[], &[], &mut stack, &McuEnv::deny_all())
        .unwrap();
    assert_eq!(ret, 0, "deny_all resolves every helper to 0");
}
