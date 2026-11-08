// SPDX-License-Identifier: (Apache-2.0 OR MIT)
//! Memory-permission regression for the default (host) build.
//!
//! The fork changed `check_mem` from `range.contains(&addr)` to a full-window
//! test. Upstream approved an access whose *first byte* sat inside a permitted
//! range even when the access ran past the end of it. That is the defect this
//! file pins, on the API the upstream path exposes.
//!
//! The `mcu` build cannot run these cases: it removes `register_allowed_memory`
//! and `execute_program`, so the equivalent coverage lives in `mcu_abi.rs`.
//! Run with the default features:
//!
//! ```text
//! cargo test --test straddle_window
//! ```

use rbpf::ebpf;

fn insn(opc: u8, dst: u8, src: u8, off: i16, imm: i32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0] = opc;
    out[1] = (src << 4) | (dst & 0x0f);
    out[2..4].copy_from_slice(&off.to_le_bytes());
    out[4..8].copy_from_slice(&imm.to_le_bytes());
    out
}

fn ld_dw_imm(dst: u8, value: u64) -> [[u8; 8]; 2] {
    [
        insn(ebpf::LD_DW_IMM, dst, 0, 0, value as u32 as i32),
        insn(0, 0, 0, 0, (value >> 32) as u32 as i32),
    ]
}

fn prog(parts: &[[u8; 8]]) -> std::vec::Vec<u8> {
    parts.iter().flat_map(|i| i.iter().copied()).collect()
}

/// A load that starts on the last permitted byte and runs past the range.
fn straddling_prog(base: u64) -> std::vec::Vec<u8> {
    let mut parts: std::vec::Vec<[u8; 8]> = std::vec::Vec::new();
    parts.extend(ld_dw_imm(1, base + 12));
    parts.push(insn(ebpf::LD_DW_REG, 0, 1, 0, 0));
    parts.push(insn(ebpf::EXIT, 0, 0, 0, 0));
    prog(&parts)
}

#[test]
fn an_8_byte_load_past_the_permitted_end_is_refused() {
    // Permit 0..16, then load 8 bytes at offset 12. The window is 12..20, and
    // its first byte is inside the permitted range. Upstream let this through.
    let buf = [0u8; 16];
    let code = straddling_prog(buf.as_ptr() as u64);
    let mut vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    let base = buf.as_ptr() as u64;
    vm.register_allowed_memory(base..base + buf.len() as u64);

    let err = vm.execute_program(&[], &[]).unwrap_err();
    assert!(
        err.to_string().contains("out of bounds"),
        "a window that ends past the permitted range must be refused, got: {err}"
    );
}

#[test]
fn the_same_load_inside_the_range_is_accepted() {
    // Control: permit 0..32 so the 12..20 window fits. The program must run.
    // Without this case, the refusal above could be caused by anything.
    let buf = [0u8; 32];
    let base = buf.as_ptr() as u64;
    let code = straddling_prog(base);
    let mut vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    vm.register_allowed_memory(base..base + buf.len() as u64);

    let ret = vm.execute_program(&[], &[]).expect("permitted load must run");
    // The loaded u64 comes from zeroed memory, so r0 is 0.
    assert_eq!(ret, 0);
}

#[test]
fn an_exactly_ending_window_is_accepted() {
    // Boundary: permit 0..16 and load 8 bytes at offset 8, which ends exactly at
    // the range end. A full-window check must accept this; an off-by-one in the
    // other direction would reject a legitimate access.
    let buf = [0u8; 16];
    let base = buf.as_ptr() as u64;
    let mut parts: std::vec::Vec<[u8; 8]> = std::vec::Vec::new();
    parts.extend(ld_dw_imm(1, base + 8));
    parts.push(insn(ebpf::LD_DW_REG, 0, 1, 0, 0));
    parts.push(insn(ebpf::EXIT, 0, 0, 0, 0));
    let code = prog(&parts);

    let mut vm = rbpf::EbpfVmMbuff::new(Some(&code)).unwrap();
    vm.register_allowed_memory(base..base + buf.len() as u64);
    vm.execute_program(&[], &[])
        .expect("a window ending exactly at the range end must run");
}
