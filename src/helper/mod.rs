//! BPF helper dispatcher (fixed IRAM trampoline).
//!
//! Dispatched: map lookup/update/delete (ARRAY/HASH), ktime_get_ns, trace_printk,
//! csum_diff, and shrink-only xdp_adjust_tail. All other IDs return `usize::MAX`
//! (unsupported), never panic. Safety obligations: SC-01..SC-08 and SC-10.

mod inner;

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

// IRAM: runs inside the XDP flight (IRQs off); flash cache misses here stall Wi-Fi.
#[esp_hal::ram]
fn dispatch_helper(idx: usize, a1: usize, a2: usize, a3: usize, a4: usize, a5: usize) -> usize {
    #[cfg(feature = "perf-trace")]
    crate::ext::perf::trace().push(
        crate::ext::perf::trace::EventKind::HelperEnter,
        idx as u32,
        0,
    );
    #[cfg(feature = "perf-aggregate")]
    crate::ext::perf::aggregate::helper_enter();
    #[cfg(feature = "perf-aggregate")]
    let is_map = matches!(
        idx,
        HelperIdx::BPF_FUNC_MAP_LOOKUP_ELEM
            | HelperIdx::BPF_FUNC_MAP_UPDATE_ELEM
            | HelperIdx::BPF_FUNC_MAP_DELETE_ELEM
    );
    #[cfg(feature = "perf-aggregate")]
    if is_map {
        crate::ext::perf::aggregate::map_enter();
    }
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
    #[cfg(feature = "perf-aggregate")]
    if is_map {
        crate::ext::perf::aggregate::map_leave();
    }
    #[cfg(feature = "perf-aggregate")]
    crate::ext::perf::aggregate::helper_leave();
    #[cfg(feature = "perf-trace")]
    crate::ext::perf::trace().push(
        crate::ext::perf::trace::EventKind::HelperLeave,
        idx as u32,
        0,
    );
    ret
}
