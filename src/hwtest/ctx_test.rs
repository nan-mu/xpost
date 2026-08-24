//! Crafted XDP contexts + multi-pass R/W checks (helpers + AOT).

use log::info;
use xdp_mcu::ctx::XdpMd;
use xdp_mcu::helper::dynptr::{BpfDynptr, ContigObj};
use xdp_mcu::helper_dispatcher;
use xdp_mcu::map::FIRMWARE_HAS_ARRAY;
use xdp_mcu::xdp;

const BPF_FUNC_XDP_ADJUST_HEAD: usize = 44;
const BPF_FUNC_XDP_LOAD_BYTES: usize = 189;
const BPF_FUNC_XDP_STORE_BYTES: usize = 190;
const BPF_FUNC_DYNPTR_WRITE: usize = 202;
const BPF_FUNC_DYNPTR_READ: usize = 201;
const BPF_FUNC_DYNPTR_DATA: usize = 203;

use xdp_mcu::redirect::XDP_PASS;

/// Payload markers beyond eth header (survive maps_helpers eth14 roundtrip).
const MARK_S1: u32 = 0x51_51_51_51;
const MARK_S2: u32 = 0x52_52_52_52;
const MARK_S3: u32 = 0x53_53_53_53;

use crate::hwtest::check;

/// Minimal Ethernet + IPv4 ethertype (34 bytes).
pub fn build_short() -> [u8; 34] {
    let mut f = [0u8; 34];
    f[0..6].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    f[6..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    f[12] = 0x08;
    f[13] = 0x00;
    f[14..18].copy_from_slice(&MARK_S1.to_le_bytes());
    f
}

/// Mid frame: 64 bytes, distinct L3 marker.
pub fn build_mid() -> [u8; 64] {
    let mut f = [0u8; 64];
    f[0..6].copy_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    f[6..12].copy_from_slice(&[0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5]);
    f[12] = 0x08;
    f[13] = 0x00;
    f[14..18].copy_from_slice(&MARK_S2.to_le_bytes());
    f[20] = 0xFE;
    f[21] = 0xED;
    f
}

/// Long frame: 128 bytes, marker deep in payload.
pub fn build_long() -> [u8; 128] {
    let mut f = [0u8; 128];
    f[0..6].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01]);
    f[6..12].copy_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x02]);
    f[12] = 0x08;
    f[13] = 0x00;
    f[14..18].copy_from_slice(&MARK_S3.to_le_bytes());
    f[64] = 0x77;
    f[65] = 0x88;
    f[66] = 0x99;
    f[67] = 0xAA;
    f
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn arr0_value() -> Option<u32> {
    if !FIRMWARE_HAS_ARRAY {
        return None;
    }
    // Optional arr_map payload only (programs that write index 0 = 42).
    None
}

/// Array-map check is skipped when `bpf.toml` has no ARRAY (e.g. firewall_ipv4).
fn arr0_ok(v: Option<u32>) -> bool {
    if FIRMWARE_HAS_ARRAY {
        v == Some(42)
    } else {
        true
    }
}

/// Contiguous-object dynptr: write, re-wrap, read — proves bytes really stuck.
pub fn run_contig_dynptr(pass: &mut u32, fail: &mut u32) {
    info!("[HWTEST] CONTIG_DYNPTR begin");
    let mut obj = [0u8; 32];
    let mut dptr = BpfDynptr::null();
    let dptr_addr = &mut dptr as *mut _ as usize;

    let fr = BpfDynptr::from_contig(ContigObj::from_slice(&mut obj), dptr_addr);
    check("contig_from", fr == 0 && dptr.is_contig(), pass, fail);

    let a = [0xA1u8, 0xA2, 0xA3, 0xA4];
    let b = [0xB1u8, 0xB2, 0xB3, 0xB4];
    let w0 = helper_dispatcher(
        BPF_FUNC_DYNPTR_WRITE,
        dptr_addr,
        0,
        a.as_ptr() as usize,
        4,
        0,
    );
    let w1 = helper_dispatcher(
        BPF_FUNC_DYNPTR_WRITE,
        dptr_addr,
        16,
        b.as_ptr() as usize,
        4,
        0,
    );
    check(
        "contig_write",
        w0 == 0 && w1 == 0 && obj[0..4] == a && obj[16..20] == b,
        pass,
        fail,
    );

    // Drop handle and re-bind the same contiguous object ("second run").
    BpfDynptr::clear(dptr_addr);
    let fr2 = BpfDynptr::from_contig(ContigObj::from_slice(&mut obj), dptr_addr);
    let mut got0 = [0u8; 4];
    let mut got1 = [0u8; 4];
    let r0 = helper_dispatcher(
        BPF_FUNC_DYNPTR_READ,
        got0.as_mut_ptr() as usize,
        4,
        dptr_addr,
        0,
        0,
    );
    let r1 = helper_dispatcher(
        BPF_FUNC_DYNPTR_READ,
        got1.as_mut_ptr() as usize,
        4,
        dptr_addr,
        16,
        0,
    );
    check(
        "contig_reread",
        fr2 == 0 && r0 == 0 && r1 == 0 && got0 == a && got1 == b,
        pass,
        fail,
    );

    // dynptr_data → mutate through raw pointer → third wrap still sees it.
    let p = helper_dispatcher(BPF_FUNC_DYNPTR_DATA, dptr_addr, 8, 4, 0, 0);
    if p != 0 && p != usize::MAX {
        unsafe {
            core::ptr::write_bytes(p as *mut u8, 0xCD, 4);
        }
    }
    BpfDynptr::clear(dptr_addr);
    let _ = BpfDynptr::from_contig(ContigObj::from_slice(&mut obj), dptr_addr);
    let mut got2 = [0u8; 4];
    let r2 = helper_dispatcher(
        BPF_FUNC_DYNPTR_READ,
        got2.as_mut_ptr() as usize,
        4,
        dptr_addr,
        8,
        0,
    );
    check(
        "contig_data_persist",
        r2 == 0 && got2 == [0xCD, 0xCD, 0xCD, 0xCD],
        pass,
        fail,
    );
    info!("[HWTEST] CONTIG_DYNPTR end");
}

/// Several crafted ctx: store → load on same pass, then load-only second pass.
pub fn run_ctx_rw_multipass(pass: &mut u32, fail: &mut u32) {
    info!("[HWTEST] CTX_RW begin");

    // --- ctx A: short ---
    {
        let mut pkt = build_short();
        let mut ctx = XdpMd::from_buf(&mut pkt);
        let ctx_ptr = &mut ctx as *mut _ as usize;
        let magic = [0xDEu8, 0xAD, 0xBE, 0xEF, 0x10, 0x20];
        let s = helper_dispatcher(
            BPF_FUNC_XDP_STORE_BYTES,
            ctx_ptr,
            0,
            magic.as_ptr() as usize,
            6,
            0,
        );
        let mut ld = [0u8; 6];
        let l = helper_dispatcher(
            BPF_FUNC_XDP_LOAD_BYTES,
            ctx_ptr,
            0,
            ld.as_mut_ptr() as usize,
            6,
            0,
        );
        check(
            "ctx_short_store_load",
            s == 0 && l == 0 && ld == magic && pkt[0..6] == magic,
            pass,
            fail,
        );
        let mut ld2 = [0u8; 6];
        let l2 = helper_dispatcher(
            BPF_FUNC_XDP_LOAD_BYTES,
            ctx_ptr,
            0,
            ld2.as_mut_ptr() as usize,
            6,
            0,
        );
        check("ctx_short_persist", l2 == 0 && ld2 == magic, pass, fail);
    }

    // --- ctx B: mid + adjust_head ---
    {
        let mut pkt = build_mid();
        let mut ctx = XdpMd::from_buf(&mut pkt);
        let ctx_ptr = &mut ctx as *mut _ as usize;
        let tag = [0x11u8, 0x22, 0x33, 0x44];
        let _ = helper_dispatcher(
            BPF_FUNC_XDP_STORE_BYTES,
            ctx_ptr,
            20,
            tag.as_ptr() as usize,
            4,
            0,
        );
        let ah = helper_dispatcher(BPF_FUNC_XDP_ADJUST_HEAD, ctx_ptr, 2, 0, 0, 0);
        let mut ld = [0u8; 4];
        let l = helper_dispatcher(
            BPF_FUNC_XDP_LOAD_BYTES,
            ctx_ptr,
            18,
            ld.as_mut_ptr() as usize,
            4,
            0,
        );
        check(
            "ctx_mid_adjust_rw",
            ah == 0 && l == 0 && ld == tag && pkt[20..24] == tag,
            pass,
            fail,
        );
    }

    // --- ctx C: long + contig dynptr over packet ---
    {
        let mut pkt = build_long();
        let mut dptr = BpfDynptr::null();
        let dptr_addr = &mut dptr as *mut _ as usize;
        let fr = BpfDynptr::from_contig(ContigObj::from_slice(&mut pkt), dptr_addr);
        let deep = [0x55u8, 0x66, 0x77, 0x88];
        let w = helper_dispatcher(
            BPF_FUNC_DYNPTR_WRITE,
            dptr_addr,
            64,
            deep.as_ptr() as usize,
            4,
            0,
        );
        BpfDynptr::clear(dptr_addr);
        let mut ctx = XdpMd::from_buf(&mut pkt);
        let ctx_ptr = &mut ctx as *mut _ as usize;
        let mut ld = [0u8; 4];
        let l = helper_dispatcher(
            BPF_FUNC_XDP_LOAD_BYTES,
            ctx_ptr,
            64,
            ld.as_mut_ptr() as usize,
            4,
            0,
        );
        check(
            "ctx_long_dynptr_then_load",
            fr == 0 && w == 0 && l == 0 && ld == deep,
            pass,
            fail,
        );
    }

    info!("[HWTEST] CTX_RW end");
}

/// Run AOT XDP multiple times on several frames; verify map + payload markers.
pub fn run_multi_xdp(pass: &mut u32, fail: &mut u32) {
    info!("[HWTEST] MULTI_XDP begin");

    // Short: two AOT calls; payload marker beyond eth must survive.
    {
        let mut frame = build_short();
        let mark_before = read_u32_le(&frame, 14);
        let r1 = xdp(&mut frame);
        let v1 = arr0_value();
        let mark_mid = read_u32_le(&frame, 14);
        let mut ctx = XdpMd::from_buf(&mut frame);
        let ctx_ptr = &mut ctx as *mut _ as usize;
        let eth_tag = [0x99u8, 0x88, 0x77, 0x66, 0x55, 0x44];
        let _ = helper_dispatcher(
            BPF_FUNC_XDP_STORE_BYTES,
            ctx_ptr,
            0,
            eth_tag.as_ptr() as usize,
            6,
            0,
        );
        let r2 = xdp(&mut frame);
        let v2 = arr0_value();
        let mark_after = read_u32_le(&frame, 14);
        check(
            "multi_xdp_short",
            r1 == XDP_PASS
                && r2 == XDP_PASS
                && arr0_ok(v1)
                && arr0_ok(v2)
                && mark_before == MARK_S1
                && mark_mid == MARK_S1
                && mark_after == MARK_S1
                && frame[0..6] == eth_tag,
            pass,
            fail,
        );
    }

    // Mid: marker + FEED bytes survive two runs.
    {
        let mut frame = build_mid();
        let r1 = xdp(&mut frame);
        let feed1 = [frame[20], frame[21]];
        frame[20] = 0x12;
        frame[21] = 0x34;
        let r2 = xdp(&mut frame);
        check(
            "multi_xdp_mid",
            r1 == XDP_PASS
                && r2 == XDP_PASS
                && feed1 == [0xFE, 0xED]
                && frame[20] == 0x12
                && frame[21] == 0x34
                && read_u32_le(&frame, 14) == MARK_S2
                && arr0_ok(arr0_value()),
            pass,
            fail,
        );
    }

    // Long: deep payload + map persistence across 3 calls.
    {
        let mut frame = build_long();
        let mut ok = true;
        for i in 0..3 {
            let rc = xdp(&mut frame);
            if rc != XDP_PASS || !arr0_ok(arr0_value()) {
                ok = false;
            }
            frame[64] = frame[64].wrapping_add(1);
            let expect = 0x77u8.wrapping_add(i + 1);
            if frame[64] != expect {
                ok = false;
            }
        }
        check(
            "multi_xdp_long",
            ok && read_u32_le(&frame, 14) == MARK_S3,
            pass,
            fail,
        );
    }

    info!("[HWTEST] MULTI_XDP end");
}

/// Always-on smoke (no hw-selftest feature): multi-ctx AOT + contig dynptr log.
pub fn smoke_multi_ctx() {
    info!("[AOT] smoke_multi_ctx begin");

    let mut obj = [0u8; 16];
    let mut dptr = BpfDynptr::null();
    let dptr_addr = &mut dptr as *mut _ as usize;
    let _ = BpfDynptr::from_contig(ContigObj::from_slice(&mut obj), dptr_addr);
    let pat = [0xC0u8, 0xFF, 0xEE, 0x01];
    let _ = helper_dispatcher(
        BPF_FUNC_DYNPTR_WRITE,
        dptr_addr,
        0,
        pat.as_ptr() as usize,
        4,
        0,
    );
    BpfDynptr::clear(dptr_addr);
    let _ = BpfDynptr::from_contig(ContigObj::from_slice(&mut obj), dptr_addr);
    let mut got = [0u8; 4];
    let _ = helper_dispatcher(
        BPF_FUNC_DYNPTR_READ,
        got.as_mut_ptr() as usize,
        4,
        dptr_addr,
        0,
        0,
    );
    info!(
        "[AOT] contig_dynptr persist={}",
        if got == pat { "ok" } else { "fail" }
    );

    for (name, mut frame) in [
        ("short", build_short().to_vec()),
        ("mid", build_mid().to_vec()),
        ("long", build_long().to_vec()),
    ] {
        let mark0 = read_u32_le(&frame, 14);
        let r1 = xdp(&mut frame);
        let v1 = arr0_value();
        let r2 = xdp(&mut frame);
        let v2 = arr0_value();
        let mark1 = read_u32_le(&frame, 14);
        info!(
            "[AOT] ctx={} r1={} r2={} arr={:?}/{:?} mark_persist={}",
            name,
            r1,
            r2,
            v1,
            v2,
            if mark0 == mark1 { "ok" } else { "fail" }
        );
    }

    info!("[AOT] smoke_multi_ctx end");
}
