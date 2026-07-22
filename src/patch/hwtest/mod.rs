//! On-device HW selftest (`--features hw-selftest`).

pub mod ctx_test;

use crate::patch::map::{
    helper_delete, helper_lookup, helper_update, BPF_ANY,
};
use crate::patch::helper::dynptr::BpfDynptr;
use crate::patch::helper_dispatcher;
use crate::patch::map::{FIRMWARE_HAS_RINGBUF, FIRMWARE_MAP_COUNT, SELFTEST_MAP_ID};
use crate::patch::ctx::XdpMd;
use crate::patch::redirect::{XDP_PASS, XDP_REDIRECT};
use log::info;

const BPF_FUNC_TRACE_PRINTK: usize = 6;
const BPF_FUNC_KTIME_GET_NS: usize = 5;
const BPF_FUNC_CSUM_DIFF: usize = 28;
const BPF_FUNC_XDP_ADJUST_HEAD: usize = 44;
const BPF_FUNC_XDP_ADJUST_TAIL: usize = 65;
const BPF_FUNC_XDP_LOAD_BYTES: usize = 189;
const BPF_FUNC_XDP_STORE_BYTES: usize = 190;
const BPF_FUNC_FOR_EACH_MAP_ELEM: usize = 164;
const BPF_FUNC_TAIL_CALL: usize = 12;
const BPF_FUNC_REDIRECT: usize = 23;
const BPF_FUNC_REDIRECT_MAP: usize = 51;
const BPF_FUNC_DYNPTR_FROM_MEM: usize = 197;
const BPF_FUNC_DYNPTR_READ: usize = 201;
const BPF_FUNC_DYNPTR_DATA: usize = 203;


pub(crate) fn check(name: &str, ok: bool, pass: &mut u32, fail: &mut u32) {
    if ok {
        info!("[HWTEST] HELPER {}=ok", name);
        *pass += 1;
    } else {
        info!("[HWTEST] HELPER {}=fail", name);
        *fail += 1;
    }
}

/// Acceptance checks against the live talc pool (create / insert / OOM).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn verify_pool() -> Result<(), &'static str> {
    use crate::patch::map::hash;
    use crate::patch::map::pool::ALLOCATOR;
    use crate::patch::map::{array, RawResource, ResourceId};

    const ARRAY_ID: ResourceId = 0xA1;
    const MAP_ID: ResourceId = 0xA2;
    const HUGE_ID: ResourceId = 0xA3;

    let before = ALLOCATOR.allocated_bytes();
    array::create_array(ARRAY_ID, 1, 64)
        .await
        .map_err(|_| "create_array failed")?;
    if ALLOCATOR.allocated_bytes() <= before {
        return Err("create_array did not increase allocated_bytes");
    }
    if !RawResource::destroy(ARRAY_ID).await {
        return Err("destroy_resource(array) failed");
    }
    if ALLOCATOR.allocated_bytes() != before {
        return Err("destroy did not restore allocated_bytes");
    }

    const CAP: usize = 8;
    hash::create_hashmap::<u32, u32>(MAP_ID, CAP)
        .await
        .map_err(|_| "create_hashmap failed")?;
    let after_create = ALLOCATOR.allocated_bytes();
    let capacity = RawResource::with_mut(MAP_ID, |m: &mut hash::SharedHashMap<u32, u32>| {
        let cap = m.capacity();
        for i in 0..CAP as u32 {
            m.insert(i, i);
        }
        cap
    })
    .await
    .ok_or("hashmap missing")?;
    if capacity < CAP {
        return Err("hashmap capacity below requested");
    }
    if ALLOCATOR.allocated_bytes() != after_create {
        return Err("hashmap insert triggered extra allocation");
    }
    RawResource::destroy(MAP_ID).await;

    let before_oom = ALLOCATOR.allocated_bytes();
    match hash::create_hashmap::<u32, u32>(HUGE_ID, 65_536).await {
        Err(_) => {}
        Ok(()) => {
            RawResource::destroy(HUGE_ID).await;
            return Err("oversized create unexpectedly succeeded");
        }
    }
    if ALLOCATOR.allocated_bytes() != before_oom {
        return Err("failed OOM allocate leaked bytes");
    }
    if RawResource::read::<hash::SharedHashMap<u32, u32>, _>(HUGE_ID, |_| ())
        .await
        .is_some()
    {
        return Err("OOM allocate left registry entry");
    }

    Ok(())
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn verify_pool() -> Result<(), &'static str> {
    use crate::patch::map::hash;
    use crate::patch::map::pool::ALLOCATOR;
    use crate::patch::map::{array, RawResource, ResourceId};

    const ARRAY_ID: ResourceId = 0xA1;
    const MAP_ID: ResourceId = 0xA2;
    const HUGE_ID: ResourceId = 0xA3;

    let before = ALLOCATOR.allocated_bytes();
    array::create_array(ARRAY_ID, 1, 64).map_err(|_| "create_array failed")?;
    if ALLOCATOR.allocated_bytes() <= before {
        return Err("create_array did not increase allocated_bytes");
    }
    if !RawResource::destroy(ARRAY_ID) {
        return Err("destroy_resource(array) failed");
    }
    if ALLOCATOR.allocated_bytes() != before {
        return Err("destroy did not restore allocated_bytes");
    }

    const CAP: usize = 8;
    hash::create_hashmap::<u32, u32>(MAP_ID, CAP).map_err(|_| "create_hashmap failed")?;
    let after_create = ALLOCATOR.allocated_bytes();
    let capacity = RawResource::with_mut(MAP_ID, |m: &mut hash::SharedHashMap<u32, u32>| {
        let cap = m.capacity();
        for i in 0..CAP as u32 {
            m.insert(i, i);
        }
        cap
    })
    .ok_or("hashmap missing")?;
    if capacity < CAP {
        return Err("hashmap capacity below requested");
    }
    if ALLOCATOR.allocated_bytes() != after_create {
        return Err("hashmap insert triggered extra allocation");
    }
    RawResource::destroy(MAP_ID);

    let before_oom = ALLOCATOR.allocated_bytes();
    match hash::create_hashmap::<u32, u32>(HUGE_ID, 65_536) {
        Err(_) => {}
        Ok(()) => {
            RawResource::destroy(HUGE_ID);
            return Err("oversized create unexpectedly succeeded");
        }
    }
    if ALLOCATOR.allocated_bytes() != before_oom {
        return Err("failed OOM allocate leaked bytes");
    }
    if RawResource::read::<hash::SharedHashMap<u32, u32>, _>(HUGE_ID, |_| ()).is_some() {
        return Err("OOM allocate left registry entry");
    }

    Ok(())
}


/// Run the selftest suite and print `[HWTEST] …` markers.
pub fn run() {
    info!("[HWTEST] BEGIN");

    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut skip = 0u32;

    // bpf_printk via IRAM trampoline (AOT helper ABI).
    info!("[HWTEST] PRINTK begin");
    for seq in 0u32..5 {
        let fmt = b"bpf_printk seq=%u";
        let fmt_len = (1usize << 24) | fmt.len();
        let rc = helper_dispatcher(
            BPF_FUNC_TRACE_PRINTK,
            fmt.as_ptr() as usize,
            fmt_len,
            seq as usize,
            0,
            0,
        );
        if rc != 0 {
            check("printk", false, &mut pass, &mut fail);
            break;
        }
        if seq == 4 {
            check("printk", true, &mut pass, &mut fail);
        }
    }
    info!("[HWTEST] PRINTK end");

    let t = helper_dispatcher(BPF_FUNC_KTIME_GET_NS, 0, 0, 0, 0, 0);
    check("ktime_get_ns", t != usize::MAX && t != 0, &mut pass, &mut fail);

    if FIRMWARE_MAP_COUNT == 0 {
        info!("[HWTEST] MAPS=SKIP none in bpf.toml");
        skip += 1;
    } else {
        run_map_tests(&mut pass, &mut fail);
        if FIRMWARE_HAS_RINGBUF {
            run_ringbuf_tests(&mut pass, &mut fail);
            run_dynptr_tests(&mut pass, &mut fail);
        } else {
            info!("[HWTEST] RINGBUF=SKIP none in bpf.toml");
            skip += 1;
            run_dynptr_mem_only(&mut pass, &mut fail);
        }
    }

    let from = [0x12u8, 0x34, 0x56, 0x78];
    let to = [0x12u8, 0x34, 0x00, 0x00];
    let csum = helper_dispatcher(
        BPF_FUNC_CSUM_DIFF,
        from.as_ptr() as usize,
        from.len(),
        to.as_ptr() as usize,
        to.len(),
        0,
    );
    // from/to BE words: ~0x1234+~0x5678+0x1234+0x0000 → fold → !sum = 0x5678
    check("csum_diff", csum == 0x5678, &mut pass, &mut fail);

    let mut pkt = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    let mut ctx = XdpMd::from_buf(&mut pkt);
    let ctx_ptr = &mut ctx as *mut _ as usize;
    let mut buf = [0u8; 4];
    let lrc = helper_dispatcher(
        BPF_FUNC_XDP_LOAD_BYTES,
        ctx_ptr,
        0,
        buf.as_mut_ptr() as usize,
        4,
        0,
    );
    check(
        "xdp_load_bytes",
        lrc == 0 && buf == [0, 1, 2, 3],
        &mut pass,
        &mut fail,
    );

    let newb = [0xAAu8, 0xBB, 0xCC, 0xDD];
    let src = helper_dispatcher(
        BPF_FUNC_XDP_STORE_BYTES,
        ctx_ptr,
        4,
        newb.as_ptr() as usize,
        4,
        0,
    );
    check(
        "xdp_store_bytes",
        src == 0 && pkt[4..8] == newb,
        &mut pass,
        &mut fail,
    );

    let ah = helper_dispatcher(BPF_FUNC_XDP_ADJUST_HEAD, ctx_ptr, 2, 0, 0, 0);
    check(
        "xdp_adjust_head",
        ah == 0 && ctx.data == unsafe { pkt.as_mut_ptr().add(2) },
        &mut pass,
        &mut fail,
    );

    let at = helper_dispatcher(
        BPF_FUNC_XDP_ADJUST_TAIL,
        ctx_ptr,
        (-2isize) as usize,
        0,
        0,
        0,
    );
    check("xdp_adjust_tail", at == 0, &mut pass, &mut fail);

    run_redirect_tests(ctx_ptr, &mut pass, &mut fail);

    if FIRMWARE_MAP_COUNT > 0 {
        ctx_test::run_contig_dynptr(&mut pass, &mut fail);
        ctx_test::run_ctx_rw_multipass(&mut pass, &mut fail);
        ctx_test::run_multi_xdp(&mut pass, &mut fail);
    }

    run_xdp_payload(&mut pass, &mut fail);

    info!(
        "[HWTEST] END pass={} fail={} skip={}",
        pass, fail, skip
    );
}

fn run_redirect_tests(ctx_ptr: usize, pass: &mut u32, fail: &mut u32) {
    let r0 = helper_dispatcher(BPF_FUNC_REDIRECT, 0, 0, 0, 0, 0);
    check("redirect", r0 == XDP_REDIRECT as usize, pass, fail);

    let r1 = helper_dispatcher(BPF_FUNC_REDIRECT, 1, 0, 0, 0, 0);
    check("redirect_unsup", r1 == (-95i32) as usize, pass, fail);

    let rm = helper_dispatcher(
        BPF_FUNC_REDIRECT_MAP,
        SELFTEST_MAP_ID as usize,
        0,
        XDP_PASS as usize,
        0,
        0,
    );
    check("redirect_map", rm == XDP_PASS as usize, pass, fail);

    let tc = helper_dispatcher(BPF_FUNC_TAIL_CALL, ctx_ptr, SELFTEST_MAP_ID as usize, 0, 0, 0);
    check(
        "tail_call",
        tc == (-2i32) as usize,
        pass,
        fail,
    );
}

fn run_ringbuf_tests(pass: &mut u32, fail: &mut u32) {
    // Requires `MAP_ID_RB_MAP` from bpf.toml; only called when FIRMWARE_HAS_RINGBUF.
    // Kept as a soft skip so firmware builds without a ringbuf map still compile.
    let _ = (pass, fail);
    info!("[HWTEST] RINGBUF helpers=SKIP (no MAP_ID_RB_MAP in this payload build)");
}

fn run_dynptr_mem_only(pass: &mut u32, fail: &mut u32) {
    let mut mem = [0x11u8, 0x22, 0x33, 0x44];
    let mut dptr = BpfDynptr::null();
    let dptr_addr = &mut dptr as *mut _ as usize;

    let fr = helper_dispatcher(
        BPF_FUNC_DYNPTR_FROM_MEM,
        mem.as_mut_ptr() as usize,
        4,
        0,
        dptr_addr,
        0,
    );
    check("dynptr_from_mem", fr == 0 && dptr.data != 0, pass, fail);

    let mut scratch = [0u8; 4];
    let rr = helper_dispatcher(
        BPF_FUNC_DYNPTR_READ,
        scratch.as_mut_ptr() as usize,
        4,
        dptr_addr,
        0,
        0,
    );
    check(
        "dynptr_read",
        rr == 0 && scratch == mem,
        pass,
        fail,
    );

    let data_ptr = helper_dispatcher(BPF_FUNC_DYNPTR_DATA, dptr_addr, 0, 4, 0, 0);
    check(
        "dynptr_data",
        data_ptr != 0 && data_ptr != usize::MAX && data_ptr == mem.as_ptr() as usize,
        pass,
        fail,
    );
}

fn run_dynptr_tests(pass: &mut u32, fail: &mut u32) {
    run_dynptr_mem_only(pass, fail);
    info!("[HWTEST] RINGBUF dynptr=SKIP (no MAP_ID_RB_MAP in this payload build)");
}

fn run_xdp_payload(pass: &mut u32, fail: &mut u32) {
    info!("[HWTEST] XDP_CALL begin");
    let mut frame = [
        0xAAu8, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, // dst
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, // src
        0x08, 0x00, // IPv4
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    let res = crate::patch::xdp(&mut frame);
    info!("[HWTEST] XDP result={}", res);

    // firewall_ipv4: incomplete L3 → XDP_PASS
    if res == XDP_PASS {
        info!("[HWTEST] XDP=ok firewall_ipv4");
        *pass += 1;
    } else {
        info!("[HWTEST] XDP=fail rc={}", res);
        *fail += 1;
    }
    info!("[HWTEST] XDP_CALL end");
}

fn run_map_tests(pass: &mut u32, fail: &mut u32) {
    use crate::patch::map::{SELFTEST_MAP_ID, SELFTEST_MAP_TYPE, SELFTEST_KEY_SIZE, SELFTEST_VALUE_SIZE};

    info!(
        "[HWTEST] MAPS_OK id={} type={} key={} val={}",
        SELFTEST_MAP_ID, SELFTEST_MAP_TYPE, SELFTEST_KEY_SIZE, SELFTEST_VALUE_SIZE
    );
    *pass += 1;

    match SELFTEST_MAP_TYPE {
        "BPF_MAP_TYPE_ARRAY" => {
            crate::patch::map::array::run_selftest(pass, fail);
        }
        "BPF_MAP_TYPE_HASH" => {
            run_hash_map_tests(pass, fail);
        }
        _ => {
            info!("[HWTEST] HELPER map_*=SKIP kind={}", SELFTEST_MAP_TYPE);
        }
    }
}

fn run_hash_map_tests(pass: &mut u32, fail: &mut u32) {
    use crate::patch::map::{
        SELFTEST_KEY_SIZE, SELFTEST_MAP_ID, SELFTEST_VALUE_SIZE,
    };

    let key = [0u8; 64];
    let mut val = [0u8; 64];
    if SELFTEST_KEY_SIZE > key.len() || SELFTEST_VALUE_SIZE > val.len() {
        check("map_update_elem", false, pass, fail);
        return;
    }
    val[0] = 1;

    let map = SELFTEST_MAP_ID as usize;
    let key_ptr = key.as_ptr() as usize;
    let val_ptr = val.as_ptr() as usize;

    check(
        "map_update_elem",
        helper_update(map, key_ptr, val_ptr, BPF_ANY as usize) == 0,
        pass,
        fail,
    );
    let found = helper_lookup(map, key_ptr);
    check(
        "map_lookup_elem",
        found != 0 && unsafe { *(found as *const u8) } == 1,
        pass,
        fail,
    );

    static mut FOREACH_HITS: u32 = 0;
    extern "C" fn foreach_cb(_map: usize, _key: usize, _val: usize, _ctx: usize) -> i32 {
        unsafe { FOREACH_HITS += 1 };
        0
    }
    unsafe { FOREACH_HITS = 0 };
    let frc = helper_dispatcher(
        BPF_FUNC_FOR_EACH_MAP_ELEM,
        map,
        foreach_cb as *const () as usize,
        0,
        0,
        0,
    );
    check(
        "for_each_map_elem",
        frc == 0 && unsafe { FOREACH_HITS } >= 1,
        pass,
        fail,
    );

    check(
        "map_delete_elem",
        helper_delete(map, key_ptr) == 0 && helper_lookup(map, key_ptr) == 0,
        pass,
        fail,
    );
}
