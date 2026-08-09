//! BPF_MAP_TYPE_RINGBUF — length-prefixed byte ring in the talc pool.

#![allow(unsafe_op_in_unsafe_fn)]

use core::alloc::Layout;
use core::ptr;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;

use crate::patch::helper::dynptr::BpfDynptr;

use super::abi::{TypedMap, resolve_map_id, with_typed_map};
use super::ops::{MapError, MapOps};
use super::pool::{HEAP_SIZE, MapKind, POOL, RawResource, ResourceId, allocator};

const MAX_RECORD_BYTES: usize = 256;

/// Byte-capacity ring with length-prefixed records (BPF ringbuf).
pub struct BpfRingBuf {
    data: Box<[u8], &'static talc::TalcCell<talc::source::Manual>>,
    capacity: usize,
    prod: usize,
    /// Outstanding reserve: (header offset, payload size).
    reserved: Option<(usize, usize)>,
}

unsafe fn drop_ring(ptr: *mut u8) {
    unsafe { super::pool::reclaim_talc_box::<BpfRingBuf>(ptr, "BpfRingBuf") };
}

fn alloc_bpf_ringbuf(byte_capacity: usize) -> Result<*mut u8, AllocError> {
    if byte_capacity < 16 || byte_capacity > HEAP_SIZE / 2 {
        return Err(AllocError);
    }
    let mut vec = allocator_api2::vec::Vec::new_in(allocator());
    vec.try_reserve_exact(byte_capacity)
        .map_err(|_| AllocError)?;
    vec.resize(byte_capacity, 0);
    let data = vec.into_boxed_slice();
    let map = BpfRingBuf {
        data,
        capacity: byte_capacity,
        prod: 0,
        reserved: None,
    };
    let boxed = Box::try_new_in(map, allocator())?;
    Ok(Box::into_raw(boxed) as *mut u8)
}

/// Create a ringbuf with `byte_capacity` data area (Linux `max_entries`).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn create_bpf_ringbuf(id: ResourceId, byte_capacity: usize) -> Result<(), AllocError> {
    let raw = alloc_bpf_ringbuf(byte_capacity)?;
    let layout = Layout::new::<BpfRingBuf>();
    RawResource::insert(id, raw, layout, drop_ring, Some(MapKind::Ringbuf), 0, 0, 0).await
}

#[cfg(feature = "patch-sync-st")]
pub fn create_bpf_ringbuf(id: ResourceId, byte_capacity: usize) -> Result<(), AllocError> {
    let raw = alloc_bpf_ringbuf(byte_capacity)?;
    let layout = Layout::new::<BpfRingBuf>();
    RawResource::insert(id, raw, layout, drop_ring, Some(MapKind::Ringbuf), 0, 0, 0)
}

impl MapOps for BpfRingBuf {
    fn lookup(&mut self, _key: *const u8) -> Result<*mut u8, MapError> {
        let e = MapError::Unsupported;
        e.log_if_bug("ringbuf.lookup");
        Err(e)
    }

    fn update(&mut self, _key: *const u8, _value: *const u8, _flags: u64) -> Result<(), MapError> {
        let e = MapError::Unsupported;
        e.log_if_bug("ringbuf.update");
        Err(e)
    }

    fn delete(&mut self, _key: *const u8) -> Result<(), MapError> {
        let e = MapError::Unsupported;
        e.log_if_bug("ringbuf.delete");
        Err(e)
    }
}

impl BpfRingBuf {
    const HDR: usize = 4;

    fn free_bytes(&self) -> usize {
        self.capacity.saturating_sub(self.prod)
    }

    /// Reserve `size` payload bytes; returns pointer to payload or null.
    pub fn reserve(&mut self, size: usize) -> *mut u8 {
        if self.reserved.is_some() || size == 0 || size > MAX_RECORD_BYTES {
            return ptr::null_mut();
        }
        let need = Self::HDR.checked_add(size).unwrap_or(usize::MAX);
        if need > self.free_bytes() {
            return ptr::null_mut();
        }
        let off = self.prod;
        let len_bytes = (size as u32).to_le_bytes();
        self.data[off..off + Self::HDR].copy_from_slice(&len_bytes);
        self.reserved = Some((off, size));
        unsafe { self.data.as_mut_ptr().add(off + Self::HDR) }
    }

    pub fn submit(&mut self) {
        if let Some((off, size)) = self.reserved.take() {
            self.prod = off + Self::HDR + size;
        }
    }

    pub fn discard(&mut self) {
        self.reserved = None;
    }

    pub fn output(&mut self, src: *const u8, size: usize) -> i32 {
        if src.is_null() || size == 0 {
            return -22;
        }
        let dst = self.reserve(size);
        if dst.is_null() {
            return -28; // ENOSPC
        }
        unsafe { ptr::copy_nonoverlapping(src, dst, size) };
        self.submit();
        0
    }

    /// Bytes produced so far (consumer not implemented; matches smoke query).
    pub fn query_avail(&self) -> usize {
        self.prod
    }

    pub(super) fn reserved_payload_addr(&self) -> Option<usize> {
        let (off, _) = self.reserved?;
        Some(unsafe { self.data.as_ptr().add(off + Self::HDR) as usize })
    }
}

fn with_ring<R>(id: ResourceId, f: impl FnOnce(&mut BpfRingBuf) -> R) -> Option<R> {
    with_typed_map(id, |typed| match typed {
        TypedMap::Ringbuf(rb) => Some(f(rb)),
        _ => None,
    })
    .flatten()
}

fn with_ring_matching<R>(data: usize, f: impl FnOnce(&mut BpfRingBuf) -> R) -> Option<R> {
    let mut guard = POOL.try_write()?;
    let table = guard.as_mut()?;
    for res in table.values_mut() {
        if res.kind != Some(MapKind::Ringbuf) {
            continue;
        }
        let rb: &mut BpfRingBuf = unsafe { res.as_mut() };
        if rb.reserved_payload_addr() == Some(data) {
            return Some(f(rb));
        }
    }
    None
}

/// Ringbuf helpers (map must be a ringbuf resource).
pub fn helper_ringbuf_reserve(map: usize, size: usize, _flags: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return 0;
    };
    with_ring(id, |rb| rb.reserve(size) as usize).unwrap_or(0)
}

pub fn helper_ringbuf_submit(data: usize) -> usize {
    with_ring_matching(data, |rb| {
        rb.submit();
        0
    })
    .unwrap_or((-22i32) as usize)
}

pub fn helper_ringbuf_discard(data: usize) -> usize {
    with_ring_matching(data, |rb| {
        rb.discard();
        0
    })
    .unwrap_or((-22i32) as usize)
}

pub fn helper_ringbuf_output(map: usize, data: usize, size: usize, _flags: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return (-22i32) as usize;
    };
    with_ring(id, |rb| rb.output(data as *const u8, size) as usize).unwrap_or((-22i32) as usize)
}

pub fn helper_ringbuf_query(map: usize, _flags: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return 0;
    };
    with_ring(id, |rb| rb.query_avail()).unwrap_or(0)
}

/// `bpf_ringbuf_reserve_dynptr(map, size, flags, ptr)` → 0 / negated errno.
pub fn helper_ringbuf_reserve_dynptr(map: usize, size: usize, _flags: usize, ptr: usize) -> usize {
    if ptr == 0 || size == 0 {
        return (-22i32) as usize;
    }
    let Some(id) = resolve_map_id(map) else {
        BpfDynptr::clear(ptr);
        return (-22i32) as usize;
    };
    let sample = with_ring(id, |rb| rb.reserve(size) as usize).unwrap_or(0);
    if sample == 0 {
        BpfDynptr::clear(ptr);
        return (-12i32) as usize; // ENOMEM
    }
    BpfDynptr::set_ringbuf(ptr, sample, size);
    0
}

/// `bpf_ringbuf_submit_dynptr(ptr, flags)`.
pub fn helper_ringbuf_submit_dynptr(ptr: usize, _flags: usize) -> usize {
    if let Some(data) = BpfDynptr::ringbuf_payload(ptr) {
        let _ = helper_ringbuf_submit(data);
    }
    BpfDynptr::clear(ptr);
    0
}

/// `bpf_ringbuf_discard_dynptr(ptr, flags)`.
pub fn helper_ringbuf_discard_dynptr(ptr: usize, _flags: usize) -> usize {
    if let Some(data) = BpfDynptr::ringbuf_payload(ptr) {
        let _ = helper_ringbuf_discard(data);
    }
    BpfDynptr::clear(ptr);
    0
}
