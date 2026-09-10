//! BPF_MAP_TYPE_ARRAY — byte storage, aligned element stride, zero-filled.
//!
//! Element type is opaque at runtime: callers pass `value_size` (sizeof value).
//! Storage is a contiguous `u8` buffer of `stride * max_entries` bytes, where
//! `stride = align_up(value_size, natural_align(value_size))`. Index `i` maps to
//! `base + i * stride`. Registry records `value_size` (copy width) and
//! `value_stride` (index step). Values are always zero-initialized — no init API.

#![allow(unsafe_op_in_unsafe_fn)]

use core::ptr;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;

use crate::map::helper::ops::{MapError, MapOps};
use crate::map::runtime::pool::{HEAP_SIZE, MapKind, RawResource, ResourceId, allocator};

/// Max single-element size (same bound as other BPF maps).
pub const MAX_ELEM_BYTES: usize = 256;

/// Dense ARRAY map: key = `u32` index, value = `value_size` bytes at `index * stride`.
pub struct BpfArrayMap {
    /// Logical sizeof(value) — bytes copied on update / exposed to helpers.
    pub value_size: usize,
    /// Aligned element pitch for address calculation (`>= value_size`).
    pub value_stride: usize,
    pub max_entries: usize,
    /// Contiguous `max_entries * value_stride` bytes, zero-initialized.
    data: Box<[u8], &'static talc::TalcCell<talc::source::Manual>>,
}

impl BpfArrayMap {
    /// Absolute pointer to element `idx`, or null if out of range.
    #[inline]
    pub fn elem_ptr(&self, idx: usize) -> *mut u8 {
        if idx >= self.max_entries {
            return ptr::null_mut();
        }
        // SAFETY: index in range; BPF may write value bytes through the pointer.
        unsafe { self.data.as_ptr().add(idx * self.value_stride) as *mut u8 }
    }

    /// Userspace: contiguous storage (read).
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Userspace: contiguous storage (hold `POOL` write lock).
    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// `bpf_for_each_map_elem` walk (retained; helper currently unsupported).
    #[allow(dead_code)]
    pub(super) fn for_each_elems(
        &mut self,
        map_arg: usize,
        cb: extern "C" fn(usize, usize, usize, usize) -> i32,
        callback_ctx: usize,
    ) -> usize {
        for idx in 0..self.max_entries {
            let mut key = idx as u32;
            let key_ptr = &mut key as *mut u32 as usize;
            let val_ptr = self.elem_ptr(idx) as usize;
            if val_ptr == 0 {
                continue;
            }
            let rc = cb(map_arg, key_ptr, val_ptr, callback_ctx);
            if rc == 1 {
                break;
            }
        }
        0
    }

    /// Natural alignment for an opaque value of `value_size` bytes (no BTF).
    ///
    /// Largest ABI-relevant power-of-two divisor among {8,4,2,1} (RV32 max = 8).
    pub fn value_align(value_size: usize) -> usize {
        if value_size == 0 {
            return 1;
        }
        if value_size % 8 == 0 {
            8
        } else if value_size % 4 == 0 {
            4
        } else if value_size % 2 == 0 {
            2
        } else {
            1
        }
    }

    /// Per-index address step: `align_up(value_size, value_align(value_size))`.
    pub fn value_stride(value_size: usize) -> usize {
        let a = Self::value_align(value_size);
        let rem = value_size % a;
        if rem == 0 {
            value_size
        } else {
            value_size + (a - rem)
        }
    }
}

impl MapOps for BpfArrayMap {
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapError> {
        if key.is_null() {
            let e = MapError::InvalidArg;
            e.log_if_bug("array.lookup");
            return Err(e);
        }
        let idx = unsafe { ptr::read_unaligned(key as *const u32) } as usize;
        let p = self.elem_ptr(idx);
        if p.is_null() {
            Err(MapError::NotFound)
        } else {
            Ok(p)
        }
    }

    fn update(&mut self, key: *const u8, value: *const u8, _flags: u64) -> Result<(), MapError> {
        if key.is_null() || value.is_null() {
            let e = MapError::InvalidArg;
            e.log_if_bug("array.update");
            return Err(e);
        }
        let idx = unsafe { ptr::read_unaligned(key as *const u32) } as usize;
        let dst = self.elem_ptr(idx);
        if dst.is_null() {
            let e = MapError::InvalidArg;
            e.log_if_bug("array.update");
            return Err(e);
        }
        unsafe { ptr::copy_nonoverlapping(value, dst, self.value_size) };
        Ok(())
    }

    fn delete(&mut self, _key: *const u8) -> Result<(), MapError> {
        let e = MapError::Unsupported;
        e.log_if_bug("array.delete");
        Err(e)
    }
}

/// Allocate ARRAY storage; returns `(raw, layout, value_size, stride)` for insert.
fn alloc_array(
    value_size: usize,
    max_entries: usize,
) -> Result<(*mut u8, core::alloc::Layout, usize, usize), AllocError> {
    if value_size == 0 || value_size > MAX_ELEM_BYTES || max_entries == 0 {
        return Err(AllocError);
    }
    let stride = BpfArrayMap::value_stride(value_size);
    let total = stride.checked_mul(max_entries).ok_or(AllocError)?;
    if total > HEAP_SIZE {
        return Err(AllocError);
    }
    let mut vec = allocator_api2::vec::Vec::new_in(allocator());
    vec.try_reserve_exact(total).map_err(|_| AllocError)?;
    vec.resize(total, 0);
    let data = vec.into_boxed_slice();
    let map = BpfArrayMap {
        value_size,
        value_stride: stride,
        max_entries,
        data,
    };
    let boxed = Box::try_new_in(map, allocator())?;
    let raw = Box::into_raw(boxed) as *mut u8;
    let layout = core::alloc::Layout::new::<BpfArrayMap>();
    Ok((raw, layout, value_size, stride))
}

unsafe fn drop_array(ptr: *mut u8) {
    unsafe { crate::map::runtime::pool::reclaim_talc_box::<BpfArrayMap>(ptr, "BpfArrayMap") };
}

/// Create a zero-filled ARRAY map and register it under `id`.
///
/// - `value_size`: sizeof(element) — struct, `u64`, `u8`, …
/// - `max_entries`: number of slots
/// - Storage: `value_stride(value_size) * max_entries` bytes, all `0`
pub(crate) async fn create_array(
    id: ResourceId,
    value_size: usize,
    max_entries: usize,
) -> Result<(), AllocError> {
    let (raw, layout, value_size, stride) = alloc_array(value_size, max_entries)?;
    RawResource::insert(
        id,
        raw,
        layout,
        drop_array,
        Some(MapKind::Array),
        4,
        value_size,
        stride,
    )
    .await
}

/// Alias used by `bpf.toml` codegen (`init_firmware_maps`).
pub async fn create_bpf_array(
    id: ResourceId,
    value_size: usize,
    max_entries: usize,
) -> Result<(), AllocError> {
    create_array(id, value_size, max_entries).await
}

pub(crate) fn alloc_array_storage(
    value_size: usize,
    max_entries: usize,
) -> Result<RawResource, AllocError> {
    let (raw, layout, value_size, stride) = alloc_array(value_size, max_entries)?;
    Ok(RawResource::build(
        raw,
        layout,
        drop_array,
        Some(MapKind::Array),
        4,
        value_size,
        stride,
    ))
}

fn check(name: &str, ok: bool, pass: &mut u32, fail: &mut u32) {
    if ok {
        log::info!("[HWTEST] HELPER {}=ok", name);
        *pass += 1;
    } else {
        log::info!("[HWTEST] HELPER {}=fail", name);
        *fail += 1;
    }
}

/// HW selftest for the first ARRAY map in bpf.toml (when present).
pub fn run_selftest(pass: &mut u32, fail: &mut u32) {
    use crate::helper_dispatcher;
    use crate::map::{BPF_ANY, helper_lookup, helper_update};
    use crate::map::{SELFTEST_KEY_SIZE, SELFTEST_MAP_ID, SELFTEST_VALUE_SIZE};
    use log::info;

    const BPF_FUNC_FOR_EACH_MAP_ELEM: usize = 164;

    info!(
        "[HWTEST] ARRAY_OK id={} value_size={} stride={}",
        SELFTEST_MAP_ID,
        SELFTEST_VALUE_SIZE,
        BpfArrayMap::value_stride(SELFTEST_VALUE_SIZE)
    );
    *pass += 1;

    let key = [0u8; 4];
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

    // Stride addressing: index 0 and (if present) index 1 are stride apart.
    if SELFTEST_VALUE_SIZE > 0 {
        let stride = BpfArrayMap::value_stride(SELFTEST_VALUE_SIZE);
        let mut guard = match crate::map::runtime::pool::pool_try_write() {
            Some(g) => g,
            None => {
                check("array_stride", false, pass, fail);
                return;
            }
        };
        let mut table = match guard.as_mut() {
            Some(t) => t,
            None => {
                check("array_stride", false, pass, fail);
                return;
            }
        };
        let mut res = match table.get_mut(&SELFTEST_MAP_ID) {
            Some(r) => r,
            None => {
                check("array_stride", false, pass, fail);
                return;
            }
        };
        let ok_meta = res.value_size() == SELFTEST_VALUE_SIZE && res.value_stride() == stride;
        let value_stride = res.value_stride();
        let m: &mut BpfArrayMap = unsafe { res.as_mut() };
        let p0 = m.elem_ptr(0) as usize;
        let p1 = m.elem_ptr(1) as usize;
        let ok_addr = p0 != 0
            && (m.max_entries < 2 || p1 == p0.wrapping_add(stride))
            && value_stride == m.value_stride;
        check("array_stride", ok_meta && ok_addr, pass, fail);
    }

    // for_each is unsupported: dispatcher fail-closed (usize::MAX), callback unused.
    extern "C" fn foreach_cb(_map: usize, _key: usize, _val: usize, _ctx: usize) -> i32 {
        0
    }
    let frc = helper_dispatcher(
        BPF_FUNC_FOR_EACH_MAP_ELEM,
        map,
        foreach_cb as *const () as usize,
        0,
        0,
        0,
    );
    check(
        "for_each_map_elem_unsupported",
        frc == usize::MAX,
        pass,
        fail,
    );

    use crate::map::helper_delete;
    let del = helper_delete(map, key_ptr);
    check(
        "map_delete_elem_array_unsupported",
        del == crate::map::MapError::Unsupported.errno() as usize,
        pass,
        fail,
    );

    // Negative: unresolved map alias must miss (null), not panic.
    check(
        "map_lookup_bad_alias",
        helper_lookup(0xdead_beef, key_ptr) == 0,
        pass,
        fail,
    );
}
