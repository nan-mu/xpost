pub mod dynptr;
mod inner;

use log::{debug, info};

use crate::patch::map::resolve_map_id;

/// Known helper ids — Linux `bpf_func_id` / `include/uapi/linux/bpf.h`.
///
/// ```ignore
/// #define BPF_FUNC_map_lookup_elem          1
/// #define BPF_FUNC_map_update_elem          2
/// #define BPF_FUNC_map_delete_elem          3
/// #define BPF_FUNC_ktime_get_ns             5
/// #define BPF_FUNC_trace_printk             6
/// #define BPF_FUNC_tail_call                12
/// #define BPF_FUNC_redirect                 23
/// #define BPF_FUNC_csum_diff                28
/// #define BPF_FUNC_xdp_adjust_head          44
/// #define BPF_FUNC_redirect_map             51
/// #define BPF_FUNC_xdp_adjust_tail          65
/// #define BPF_FUNC_ringbuf_output           130
/// #define BPF_FUNC_ringbuf_reserve          131
/// #define BPF_FUNC_ringbuf_submit           132
/// #define BPF_FUNC_ringbuf_discard          133
/// #define BPF_FUNC_ringbuf_query            134
/// #define BPF_FUNC_for_each_map_elem        164
/// #define BPF_FUNC_xdp_load_bytes           189
/// #define BPF_FUNC_xdp_store_bytes          190
/// #define BPF_FUNC_dynptr_from_mem          197
/// #define BPF_FUNC_ringbuf_reserve_dynptr   198
/// #define BPF_FUNC_ringbuf_submit_dynptr    199
/// #define BPF_FUNC_ringbuf_discard_dynptr   200
/// #define BPF_FUNC_dynptr_read              201
/// #define BPF_FUNC_dynptr_write             202
/// #define BPF_FUNC_dynptr_data              203
/// ```
///
/// Call ABI: `safe_helper_call(idx, a1..a5)`; on RV32, u64 helper fields use the low 32 bits.
struct HelperIdx;

impl HelperIdx {
    const BPF_FUNC_MAP_LOOKUP_ELEM: usize = 1;
    const BPF_FUNC_MAP_UPDATE_ELEM: usize = 2;
    const BPF_FUNC_MAP_DELETE_ELEM: usize = 3;
    const BPF_FUNC_KTIME_GET_NS: usize = 5;
    const BPF_FUNC_TRACE_PRINTK: usize = 6;
    const BPF_FUNC_TAIL_CALL: usize = 12;
    const BPF_FUNC_REDIRECT: usize = 23;
    const BPF_FUNC_CSUM_DIFF: usize = 28;
    const BPF_FUNC_XDP_ADJUST_HEAD: usize = 44;
    const BPF_FUNC_REDIRECT_MAP: usize = 51;
    const BPF_FUNC_XDP_ADJUST_TAIL: usize = 65;
    const BPF_FUNC_RINGBUF_OUTPUT: usize = 130;
    const BPF_FUNC_RINGBUF_RESERVE: usize = 131;
    const BPF_FUNC_RINGBUF_SUBMIT: usize = 132;
    const BPF_FUNC_RINGBUF_DISCARD: usize = 133;
    const BPF_FUNC_RINGBUF_QUERY: usize = 134;
    const BPF_FUNC_FOR_EACH_MAP_ELEM: usize = 164;
    const BPF_FUNC_XDP_LOAD_BYTES: usize = 189;
    const BPF_FUNC_XDP_STORE_BYTES: usize = 190;
    const BPF_FUNC_DYNPTR_FROM_MEM: usize = 197;
    const BPF_FUNC_RINGBUF_RESERVE_DYNPTR: usize = 198;
    const BPF_FUNC_RINGBUF_SUBMIT_DYNPTR: usize = 199;
    const BPF_FUNC_RINGBUF_DISCARD_DYNPTR: usize = 200;
    const BPF_FUNC_DYNPTR_READ: usize = 201;
    const BPF_FUNC_DYNPTR_WRITE: usize = 202;
    const BPF_FUNC_DYNPTR_DATA: usize = 203;
}

fn helper_name(idx: usize) -> &'static str {
    match idx {
        HelperIdx::BPF_FUNC_MAP_LOOKUP_ELEM => "map_lookup_elem",
        HelperIdx::BPF_FUNC_MAP_UPDATE_ELEM => "map_update_elem",
        HelperIdx::BPF_FUNC_MAP_DELETE_ELEM => "map_delete_elem",
        HelperIdx::BPF_FUNC_KTIME_GET_NS => "ktime_get_ns",
        HelperIdx::BPF_FUNC_TRACE_PRINTK => "trace_printk",
        HelperIdx::BPF_FUNC_TAIL_CALL => "tail_call",
        HelperIdx::BPF_FUNC_REDIRECT => "redirect",
        HelperIdx::BPF_FUNC_CSUM_DIFF => "csum_diff",
        HelperIdx::BPF_FUNC_XDP_ADJUST_HEAD => "xdp_adjust_head",
        HelperIdx::BPF_FUNC_REDIRECT_MAP => "redirect_map",
        HelperIdx::BPF_FUNC_XDP_ADJUST_TAIL => "xdp_adjust_tail",
        HelperIdx::BPF_FUNC_RINGBUF_OUTPUT => "ringbuf_output",
        HelperIdx::BPF_FUNC_RINGBUF_RESERVE => "ringbuf_reserve",
        HelperIdx::BPF_FUNC_RINGBUF_SUBMIT => "ringbuf_submit",
        HelperIdx::BPF_FUNC_RINGBUF_DISCARD => "ringbuf_discard",
        HelperIdx::BPF_FUNC_RINGBUF_QUERY => "ringbuf_query",
        HelperIdx::BPF_FUNC_FOR_EACH_MAP_ELEM => "for_each_map_elem",
        HelperIdx::BPF_FUNC_XDP_LOAD_BYTES => "xdp_load_bytes",
        HelperIdx::BPF_FUNC_XDP_STORE_BYTES => "xdp_store_bytes",
        HelperIdx::BPF_FUNC_DYNPTR_FROM_MEM => "dynptr_from_mem",
        HelperIdx::BPF_FUNC_RINGBUF_RESERVE_DYNPTR => "ringbuf_reserve_dynptr",
        HelperIdx::BPF_FUNC_RINGBUF_SUBMIT_DYNPTR => "ringbuf_submit_dynptr",
        HelperIdx::BPF_FUNC_RINGBUF_DISCARD_DYNPTR => "ringbuf_discard_dynptr",
        HelperIdx::BPF_FUNC_DYNPTR_READ => "dynptr_read",
        HelperIdx::BPF_FUNC_DYNPTR_WRITE => "dynptr_write",
        HelperIdx::BPF_FUNC_DYNPTR_DATA => "dynptr_data",
        _ => "unknown",
    }
}

/// Format usize for helper debug logs — decimal avoids espflash monitor addr2line spam on `0x…`.
#[inline]
fn log_u(v: usize) -> usize {
    v
}

/// Format helper return: pointers as `0x…`, errno-style values as signed `i32`.
fn fmt_ret(ret: usize) -> (&'static str, i32) {
    if ret == usize::MAX {
        return ("err", -1);
    }
    if ret == 0 {
        return ("ok", 0);
    }
    // Linux helpers return pointers or small non-negative values; errors are negated errno.
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
/// Address comes from `mem.toml` → [`crate::patch::mem_map::HELPER_DISPATCHER_BASE`].
/// Keep this body a pure tail-call into flash so the IRAM window stays a thin
/// trampoline (full match bodies belong in `dispatch_helper`).
#[unsafe(no_mangle)]
#[unsafe(link_section = ".iram1.helper_dispatcher")]
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
            | HelperIdx::BPF_FUNC_FOR_EACH_MAP_ELEM
            | HelperIdx::BPF_FUNC_REDIRECT_MAP
            | HelperIdx::BPF_FUNC_RINGBUF_OUTPUT
            | HelperIdx::BPF_FUNC_RINGBUF_RESERVE
            | HelperIdx::BPF_FUNC_RINGBUF_SUBMIT
            | HelperIdx::BPF_FUNC_RINGBUF_DISCARD
            | HelperIdx::BPF_FUNC_RINGBUF_QUERY
            | HelperIdx::BPF_FUNC_RINGBUF_RESERVE_DYNPTR
            | HelperIdx::BPF_FUNC_RINGBUF_SUBMIT_DYNPTR
            | HelperIdx::BPF_FUNC_RINGBUF_DISCARD_DYNPTR
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
        HelperIdx::BPF_FUNC_TAIL_CALL => inner::tail_call::inner(a1, a2, a3),
        HelperIdx::BPF_FUNC_REDIRECT => inner::redirect::inner(a1, a2),
        HelperIdx::BPF_FUNC_CSUM_DIFF => inner::csum_diff::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_XDP_ADJUST_HEAD => inner::xdp_adjust_head::inner(a1, a2),
        HelperIdx::BPF_FUNC_REDIRECT_MAP => inner::redirect_map::inner(a1, a2, a3),
        HelperIdx::BPF_FUNC_XDP_ADJUST_TAIL => inner::xdp_adjust_tail::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_OUTPUT => inner::ringbuf_output::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_RINGBUF_RESERVE => inner::ringbuf_reserve::inner(a1, a2, a3),
        HelperIdx::BPF_FUNC_RINGBUF_SUBMIT => inner::ringbuf_submit::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_DISCARD => inner::ringbuf_discard::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_QUERY => inner::ringbuf_query::inner(a1, a2),
        HelperIdx::BPF_FUNC_FOR_EACH_MAP_ELEM => inner::for_each_map_elem::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_XDP_LOAD_BYTES => inner::xdp_load_bytes::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_XDP_STORE_BYTES => inner::xdp_store_bytes::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_DYNPTR_FROM_MEM => inner::dynptr_from_mem::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_RINGBUF_RESERVE_DYNPTR => {
            inner::ringbuf_reserve_dynptr::inner(a1, a2, a3, a4)
        }
        HelperIdx::BPF_FUNC_RINGBUF_SUBMIT_DYNPTR => inner::ringbuf_submit_dynptr::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_DISCARD_DYNPTR => inner::ringbuf_discard_dynptr::inner(a1, a2),
        HelperIdx::BPF_FUNC_DYNPTR_READ => inner::dynptr_read::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_DYNPTR_WRITE => inner::dynptr_write::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_DYNPTR_DATA => inner::dynptr_data::inner(a1, a2, a3),
        _ => usize::MAX,
    };
    let (ret_kind, ret_val) = fmt_ret(ret);
    if is_map_helper(idx) {
        let map_id = resolve_map_id(a1);
        match map_id {
            Some(id) => info!(
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
            None => info!(
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
