//! BPF_MAP_TYPE_ARRAY — byte storage, aligned element stride, zero-filled.

#![allow(unsafe_op_in_unsafe_fn)]

use core::ptr;

use allocator_api2::{alloc::AllocError, boxed::Box};

use crate::map::{
    MapType,
    heap::{HEAP_SIZE, RawResource, allocator},
    ops::{MapOpError, MapOps},
};

/// Max single-element size (same bound as other BPF maps).
pub(crate) const MAX_ELEM_BYTES: usize = 256;

/// Dense ARRAY map: key = `u32` index, value = `value_size` bytes at `index * stride`.
pub(crate) struct BpfArrayMap {
    /// Logical sizeof(value) — bytes copied on update / exposed to helpers.
    pub(crate) value_size: usize,
    /// Aligned element pitch for address calculation (`>= value_size`).
    pub(crate) value_stride: usize,
    pub(crate) max_entries: usize,
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
        unsafe { self.data.as_ptr().add(idx * self.value_stride) as *mut u8 }
    }

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[inline]
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Natural alignment for an opaque value of `value_size` bytes (no BTF).
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
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapOpError> {
        if key.is_null() {
            return Err(MapOpError::InvalidArg);
        }
        let idx = unsafe { ptr::read_unaligned(key as *const u32) } as usize;
        let p = self.elem_ptr(idx);
        if p.is_null() {
            Err(MapOpError::NotFound)
        } else {
            Ok(p)
        }
    }

    fn update(&mut self, key: *const u8, value: *const u8, _flags: u64) -> Result<(), MapOpError> {
        if key.is_null() || value.is_null() {
            return Err(MapOpError::InvalidArg);
        }
        let idx = unsafe { ptr::read_unaligned(key as *const u32) } as usize;
        let dst = self.elem_ptr(idx);
        if dst.is_null() {
            return Err(MapOpError::InvalidArg);
        }
        unsafe { ptr::copy_nonoverlapping(value, dst, self.value_size) };
        Ok(())
    }

    fn delete(&mut self, _key: *const u8) -> Result<(), MapOpError> {
        Err(MapOpError::Unsupported)
    }
}

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
    unsafe { crate::map::heap::reclaim_talc_box::<BpfArrayMap>(ptr, "BpfArrayMap") };
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
        Some(MapType::Array),
        4,
        value_size,
        stride,
    ))
}
