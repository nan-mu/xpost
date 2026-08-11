//! On-device HW selftest (`config.toml` → `features.hw_selftest` → `fw_us_hw_selftest`).

pub mod ctx_test;
mod probe_hash;
mod probe_ring;

use crate::patch::ctx::{IFINDEX_WIFI, IFINDEX_WIRED, XdpMd};
use crate::patch::helper::dynptr::BpfDynptr;
use crate::patch::helper_dispatcher;
use crate::patch::map::{BPF_ANY, helper_delete, helper_lookup, helper_update};
use crate::patch::map::{
    FIRMWARE_HAS_DEVMAP, FIRMWARE_HAS_RINGBUF, FIRMWARE_MAP_COUNT, SELFTEST_MAP_ID,
};
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
    use crate::patch::map::pool::ALLOCATOR;
    use crate::patch::map::{RawResource, ResourceId, create_bpf_array};

    use self::probe_hash::{ProbeHashMap, create_probe_hashmap};
    use self::probe_ring::{ProbeRingBuf, create_probe_ringbuf};

    const ARRAY_ID: ResourceId = 0xA1;
    const HASH_ID: ResourceId = 0xA2;
    const HUGE_ID: ResourceId = 0xA3;
    const RING_ID: ResourceId = 0xA4;

    let before = ALLOCATOR.allocated_bytes();
    create_bpf_array(ARRAY_ID, 1, 64)
        .await
        .map_err(|_| "create_bpf_array failed")?;
    if ALLOCATOR.allocated_bytes() <= before {
        return Err("create_bpf_array did not increase allocated_bytes");
    }
    if !RawResource::destroy(ARRAY_ID).await {
        return Err("destroy_resource(array) failed");
    }
    if ALLOCATOR.allocated_bytes() != before {
        return Err("destroy did not restore allocated_bytes");
    }

    const CAP: usize = 8;
    create_probe_hashmap::<u32, u32>(HASH_ID, CAP)
        .await
        .map_err(|_| "create_probe_hashmap failed")?;
    let after_create = ALLOCATOR.allocated_bytes();
    let capacity = RawResource::with_mut(HASH_ID, |m: &mut ProbeHashMap<u32, u32>| {
        let cap = m.capacity();
        for i in 0..CAP as u32 {
            m.insert(i, i);
        }
        cap
    })
    .await
    .ok_or("probe hashmap missing")?;
    if capacity < CAP {
        return Err("probe hashmap capacity below requested");
    }
    if ALLOCATOR.allocated_bytes() != after_create {
        return Err("probe hashmap insert triggered extra allocation");
    }
    RawResource::destroy(HASH_ID).await;

    let before_oom = ALLOCATOR.allocated_bytes();
    match create_probe_hashmap::<u32, u32>(HUGE_ID, 65_536).await {
        Err(_) => {}
        Ok(()) => {
            RawResource::destroy(HUGE_ID).await;
            return Err("oversized create unexpectedly succeeded");
        }
    }
    if ALLOCATOR.allocated_bytes() != before_oom {
        return Err("failed OOM allocate leaked bytes");
    }
    if RawResource::read::<ProbeHashMap<u32, u32>, _>(HUGE_ID, |_| ())
        .await
        .is_some()
    {
        return Err("OOM allocate left registry entry");
    }

    let before_ring = ALLOCATOR.allocated_bytes();
    create_probe_ringbuf::<u8>(RING_ID, 64)
        .await
        .map_err(|_| "create_probe_ringbuf failed")?;
    if ALLOCATOR.allocated_bytes() <= before_ring {
        return Err("create_probe_ringbuf did not increase allocated_bytes");
    }
    if RawResource::read::<ProbeRingBuf<u8>, _>(RING_ID, |_| ())
        .await
        .is_none()
    {
        return Err("probe ringbuf missing after create");
    }
    if !RawResource::destroy(RING_ID).await {
        return Err("destroy_resource(ring) failed");
    }
    if ALLOCATOR.allocated_bytes() != before_ring {
        return Err("destroy ring did not restore allocated_bytes");
    }

    Ok(())
}

#[cfg(feature = "patch-sync-st")]
pub fn verify_pool() -> Result<(), &'static str> {
    use crate::patch::map::pool::ALLOCATOR;
    use crate::patch::map::{RawResource, ResourceId, create_bpf_array};

    use self::probe_hash::{ProbeHashMap, create_probe_hashmap};
    use self::probe_ring::{ProbeRingBuf, create_probe_ringbuf};

    const ARRAY_ID: ResourceId = 0xA1;
    const HASH_ID: ResourceId = 0xA2;
    const HUGE_ID: ResourceId = 0xA3;
    const RING_ID: ResourceId = 0xA4;

    let before = ALLOCATOR.allocated_bytes();
    create_bpf_array(ARRAY_ID, 1, 64).map_err(|_| "create_bpf_array failed")?;
    if ALLOCATOR.allocated_bytes() <= before {
        return Err("create_bpf_array did not increase allocated_bytes");
    }
    if !RawResource::destroy(ARRAY_ID) {
        return Err("destroy_resource(array) failed");
    }
    if ALLOCATOR.allocated_bytes() != before {
        return Err("destroy did not restore allocated_bytes");
    }

    const CAP: usize = 8;
    create_probe_hashmap::<u32, u32>(HASH_ID, CAP).map_err(|_| "create_probe_hashmap failed")?;
    let after_create = ALLOCATOR.allocated_bytes();
    let capacity = RawResource::with_mut(HASH_ID, |m: &mut ProbeHashMap<u32, u32>| {
        let cap = m.capacity();
        for i in 0..CAP as u32 {
            m.insert(i, i);
        }
        cap
    })
    .ok_or("probe hashmap missing")?;
    if capacity < CAP {
        return Err("probe hashmap capacity below requested");
    }
    if ALLOCATOR.allocated_bytes() != after_create {
        return Err("probe hashmap insert triggered extra allocation");
    }
    RawResource::destroy(HASH_ID);

    let before_oom = ALLOCATOR.allocated_bytes();
    match create_probe_hashmap::<u32, u32>(HUGE_ID, 65_536) {
        Err(_) => {}
        Ok(()) => {
            RawResource::destroy(HUGE_ID);
            return Err("oversized create unexpectedly succeeded");
        }
    }
    if ALLOCATOR.allocated_bytes() != before_oom {
        return Err("failed OOM allocate leaked bytes");
    }
    if RawResource::read::<ProbeHashMap<u32, u32>, _>(HUGE_ID, |_| ()).is_some() {
        return Err("OOM allocate left registry entry");
    }

    let before_ring = ALLOCATOR.allocated_bytes();
    create_probe_ringbuf::<u8>(RING_ID, 64).map_err(|_| "create_probe_ringbuf failed")?;
    if ALLOCATOR.allocated_bytes() <= before_ring {
        return Err("create_probe_ringbuf did not increase allocated_bytes");
    }
    if RawResource::read::<ProbeRingBuf<u8>, _>(RING_ID, |_| ()).is_none() {
        return Err("probe ringbuf missing after create");
    }
    if !RawResource::destroy(RING_ID) {
        return Err("destroy_resource(ring) failed");
    }
    if ALLOCATOR.allocated_bytes() != before_ring {
        return Err("destroy ring did not restore allocated_bytes");
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
    check(
        "ktime_get_ns",
        t != usize::MAX && t != 0,
        &mut pass,
        &mut fail,
    );

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

    info!("[HWTEST] END pass={} fail={} skip={}", pass, fail, skip);
}

fn run_redirect_tests(ctx_ptr: usize, pass: &mut u32, fail: &mut u32) {
    let r0 = helper_dispatcher(BPF_FUNC_REDIRECT, IFINDEX_WIFI as usize, 0, 0, 0, 0);
    check("redirect", r0 == XDP_REDIRECT as usize, pass, fail);

    let r1 = helper_dispatcher(BPF_FUNC_REDIRECT, IFINDEX_WIRED as usize, 0, 0, 0, 0);
    check("redirect_unsup", r1 == (-95i32) as usize, pass, fail);

    if FIRMWARE_HAS_DEVMAP {
        use crate::patch::map::MAP_ID_TX_PORT_MAP;
        let key = 0u32;
        let ifindex = IFINDEX_WIFI;
        let map = MAP_ID_TX_PORT_MAP as usize;
        let _ = helper_update(
            map,
            (&raw const key) as *const u8 as usize,
            (&raw const ifindex) as *const u8 as usize,
            BPF_ANY as usize,
        );
        let rm = helper_dispatcher(
            BPF_FUNC_REDIRECT_MAP,
            map,
            key as usize,
            XDP_PASS as usize,
            0,
            0,
        );
        check("redirect_map", rm == XDP_REDIRECT as usize, pass, fail);
    } else {
        let rm = helper_dispatcher(
            BPF_FUNC_REDIRECT_MAP,
            SELFTEST_MAP_ID as usize,
            0,
            XDP_PASS as usize,
            0,
            0,
        );
        check("redirect_map", rm == XDP_PASS as usize, pass, fail);
    }

    let tc = helper_dispatcher(
        BPF_FUNC_TAIL_CALL,
        ctx_ptr,
        SELFTEST_MAP_ID as usize,
        0,
        0,
        0,
    );
    check("tail_call_empty", tc == (-2i32) as usize, pass, fail);

    // Prog table round-trip (no transfer — XDP_FRAME unset in helper-only tests).
    {
        use crate::patch::prog_table::{self, SlotId};
        let _ = prog_table::set(1, Some(0x4038_1000));
        check(
            "prog_table_set_get",
            prog_table::get(1) == Some(0x4038_1000),
            pass,
            fail,
        );
        prog_table::clear(1);
        check("prog_table_clear", prog_table::get(1).is_none(), pass, fail);
        let _ = SlotId::B;
    }
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
    check("dynptr_read", rr == 0 && scratch == mem, pass, fail);

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

    // vip_l2_redirect: zero daddr / unseeded VIP → XDP_PASS
    if res == XDP_PASS {
        info!("[HWTEST] XDP=ok vip_l2_redirect PASS");
        *pass += 1;
    } else {
        info!("[HWTEST] XDP=fail rc={}", res);
        *fail += 1;
    }
    info!("[HWTEST] XDP_CALL end");
}

fn run_map_tests(pass: &mut u32, fail: &mut u32) {
    use crate::patch::map::{
        SELFTEST_KEY_SIZE, SELFTEST_MAP_ID, SELFTEST_MAP_TYPE, SELFTEST_VALUE_SIZE,
    };

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
    use crate::patch::map::{SELFTEST_KEY_SIZE, SELFTEST_MAP_ID, SELFTEST_VALUE_SIZE};

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
