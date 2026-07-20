mod bpf_csum_diff;
mod bpf_dynptr_data;
mod bpf_dynptr_from_mem;
mod bpf_dynptr_read;
mod bpf_dynptr_write;
mod bpf_for_each_map_elem;
mod bpf_ktime_get_ns;
mod bpf_map_delete_elem;
mod bpf_map_lookup_elem;
mod bpf_map_update_elem;
mod bpf_printk;
mod bpf_redirect;
mod bpf_redirect_map;
mod bpf_ringbuf_discard;
mod bpf_ringbuf_discard_dynptr;
mod bpf_ringbuf_output;
mod bpf_ringbuf_query;
mod bpf_ringbuf_reserve;
mod bpf_ringbuf_reserve_dynptr;
mod bpf_ringbuf_submit;
mod bpf_ringbuf_submit_dynptr;
mod bpf_tail_call;
mod bpf_xdp_adjust_head;
mod bpf_xdp_adjust_tail;
mod bpf_xdp_load_bytes;
mod bpf_xdp_store_bytes;

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

#[unsafe(no_mangle)]
#[unsafe(link_section = ".iram1.helper_dispatcher")]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn helper_dispatcher(
    idx: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> usize {
    match idx {
        HelperIdx::BPF_FUNC_MAP_LOOKUP_ELEM => bpf_map_lookup_elem::inner(a1, a2),
        HelperIdx::BPF_FUNC_MAP_UPDATE_ELEM => bpf_map_update_elem::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_MAP_DELETE_ELEM => bpf_map_delete_elem::inner(a1, a2),
        HelperIdx::BPF_FUNC_KTIME_GET_NS => bpf_ktime_get_ns::inner(),
        HelperIdx::BPF_FUNC_TRACE_PRINTK => bpf_printk::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_TAIL_CALL => bpf_tail_call::inner(a1, a2, a3),
        HelperIdx::BPF_FUNC_REDIRECT => bpf_redirect::inner(a1, a2),
        HelperIdx::BPF_FUNC_CSUM_DIFF => bpf_csum_diff::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_XDP_ADJUST_HEAD => bpf_xdp_adjust_head::inner(a1, a2),
        HelperIdx::BPF_FUNC_REDIRECT_MAP => bpf_redirect_map::inner(a1, a2, a3),
        HelperIdx::BPF_FUNC_XDP_ADJUST_TAIL => bpf_xdp_adjust_tail::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_OUTPUT => bpf_ringbuf_output::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_RINGBUF_RESERVE => bpf_ringbuf_reserve::inner(a1, a2, a3),
        HelperIdx::BPF_FUNC_RINGBUF_SUBMIT => bpf_ringbuf_submit::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_DISCARD => bpf_ringbuf_discard::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_QUERY => bpf_ringbuf_query::inner(a1, a2),
        HelperIdx::BPF_FUNC_FOR_EACH_MAP_ELEM => bpf_for_each_map_elem::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_XDP_LOAD_BYTES => bpf_xdp_load_bytes::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_XDP_STORE_BYTES => bpf_xdp_store_bytes::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_DYNPTR_FROM_MEM => bpf_dynptr_from_mem::inner(a1, a2, a3, a4),
        HelperIdx::BPF_FUNC_RINGBUF_RESERVE_DYNPTR => {
            bpf_ringbuf_reserve_dynptr::inner(a1, a2, a3, a4)
        }
        HelperIdx::BPF_FUNC_RINGBUF_SUBMIT_DYNPTR => bpf_ringbuf_submit_dynptr::inner(a1, a2),
        HelperIdx::BPF_FUNC_RINGBUF_DISCARD_DYNPTR => bpf_ringbuf_discard_dynptr::inner(a1, a2),
        HelperIdx::BPF_FUNC_DYNPTR_READ => bpf_dynptr_read::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_DYNPTR_WRITE => bpf_dynptr_write::inner(a1, a2, a3, a4, a5),
        HelperIdx::BPF_FUNC_DYNPTR_DATA => bpf_dynptr_data::inner(a1, a2, a3),
        _ => usize::MAX,
    }
}
