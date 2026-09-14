//! BPF helper dispatcher (fixed IRAM trampoline).
//!
//! Dispatched: map lookup/update/delete (ARRAY/HASH), ktime_get_ns, trace_printk,
//! csum_diff, and shrink-only xdp_adjust_tail. All other IDs return `usize::MAX`
//! (unsupported), never panic. Safety obligations: SC-01..SC-08 and SC-10.

mod inner;

use log::debug;

use crate::{map::resolve_map_id, trace_log};

/// Known helper ids — Linux `bpf_func_id` / `include/uapi/linux/bpf.h`.
///
/// Deleted / unsupported: tail_call(12), bpf_redirect(23), xdp_adjust_head(44),
/// redirect_map(51), xdp_load/store_bytes(189/190), dynptr (197–203),
/// for_each_map_elem(164), xpost_evt_wake(225).
///
/// Supported with subset: xdp_adjust_tail(65) — shrink only (delta ≤ 0).
///
/// Deferred (file structure retained, not dispatched): BPF ringbuf (130–134).
struct HelperIdx;

impl HelperIdx {
    const BPF_FUNC_MAP_LOOKUP_ELEM: usize = 1;
    const BPF_FUNC_MAP_UPDATE_ELEM: usize = 2;
    const BPF_FUNC_MAP_DELETE_ELEM: usize = 3;
    const BPF_FUNC_KTIME_GET_NS: usize = 5;
    const BPF_FUNC_TRACE_PRINTK: usize = 6;
    const BPF_FUNC_CSUM_DIFF: usize = 28;
    const BPF_FUNC_XDP_ADJUST_TAIL: usize = 65;
}

fn helper_name(idx: usize) -> &'static str {
    match idx {
        HelperIdx::BPF_FUNC_MAP_LOOKUP_ELEM => "map_lookup_elem",
        HelperIdx::BPF_FUNC_MAP_UPDATE_ELEM => "map_update_elem",
        HelperIdx::BPF_FUNC_MAP_DELETE_ELEM => "map_delete_elem",
        HelperIdx::BPF_FUNC_KTIME_GET_NS => "ktime_get_ns",
        HelperIdx::BPF_FUNC_TRACE_PRINTK => "trace_printk",
        HelperIdx::BPF_FUNC_CSUM_DIFF => "csum_diff",
        HelperIdx::BPF_FUNC_XDP_ADJUST_TAIL => "xdp_adjust_tail",
        _ => "unsupported",
    }
}

#[inline]
fn log_u(v: usize) -> usize {
    v
}

fn fmt_ret(ret: usize) -> (&'static str, i32) {
    if ret == usize::MAX {
        return ("err", -1);
    }
    if ret == 0 {
        return ("ok", 0);
    }
    if ret > 0x3f00_0000 {
        ("ptr", ret as i32)
    } else if ret >= 0x8000_0000 {
        ("err", ret as i32)
    } else {
        ("ok", ret as i32)
    }
}

/// Fixed-address IRAM entry for AOT helper calls.
///
/// Must stay `pub` + `#[no_mangle]`: libbpf-mcu / the linker resolve this **symbol
/// by name and section** (`.xdp.helper_dispatcher`). It is an ABI trampoline, not
/// an integrator-facing API — hence [`doc(hidden)`]. Prefer never calling it from
/// firmware application code; use [`crate::map`] / [`crate::xdp`] instead.
#[doc(hidden)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.helper_dispatcher")]
#[inline(never)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn helper_dispatcher(
    idx: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> usize {
    dispatch_helper(idx, a1, a2, a3, a4, a5)
}

fn is_map_helper(idx: usize) -> bool {
    matches!(
        idx,
        HelperIdx::BPF_FUNC_MAP_LOOKUP_ELEM
            | HelperIdx::BPF_FUNC_MAP_UPDATE_ELEM
            | HelperIdx::BPF_FUNC_MAP_DELETE_ELEM
    )
}

#[inline(never)]
fn dispatch_helper(idx: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> usize {
    let ret = match idx {
        HelperIdx::BPF_FUNC_MAP_LOOKUP_ELEM => inner::map_lookup_elem::inner(a1, a2),
        HelperIdx::BPF_FUNC_MAP_UPDATE_ELEM => inner::map_update_elem::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_MAP_DELETE_ELEM => inner::map_delete_elem::inner(a1, a2),
        HelperIdx::BPF_FUNC_KTIME_GET_NS => inner::ktime_get_ns::inner(),
        HelperIdx::BPF_FUNC_TRACE_PRINTK => inner::printk::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_CSUM_DIFF => inner::csum_diff::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_XDP_ADJUST_TAIL => inner::xdp_adjust_tail::inner(a1, a2),
        // Deleted / deferred / unsupported helpers: fail closed.
        _ => usize::MAX,
    };
    let (ret_kind, ret_val) = fmt_ret(ret);
    if is_map_helper(idx) {
        let map_id = resolve_map_id(a1);
        match map_id {
            Some(id) => trace_log!(
                "[Helper][MAP] {} (#{}) map_id={} a2={} a3={} a4={} a5={} -> {}={}",
                helper_name(idx),
                idx,
                id,
                log_u(a2),
                log_u(a3),
                log_u(a4),
                log_u(a5),
                ret_kind,
                ret_val,
            ),
            None => trace_log!(
                "[Helper][MAP] {} (#{}) map_arg={} a2={} a3={} a4={} a5={} -> {}={}",
                helper_name(idx),
                idx,
                log_u(a1),
                log_u(a2),
                log_u(a3),
                log_u(a4),
                log_u(a5),
                ret_kind,
                ret_val,
            ),
        }
    } else {
        debug!(
            "bpf {} (#{}) a1={} a2={} a3={} a4={} a5={} -> {}={}",
            helper_name(idx),
            idx,
            log_u(a1),
            log_u(a2),
            log_u(a3),
            log_u(a4),
            log_u(a5),
            ret_kind,
            ret_val,
        );
    }
    ret
}
