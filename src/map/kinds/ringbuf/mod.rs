//! BPF ringbuf TX + async-ringbuf RX.
//!
//! Incomplete — enabling Cargo feature `ringbuf` fails with `compile_error!`.
//! Source retained for later implementation; remove the `compile_error!` when ready.
//! BPF helpers (`bpf_ringbuf_*`) remain deferred / not dispatched.

#![allow(unsafe_op_in_unsafe_fn)]

compile_error!("feature `ringbuf` is not implemented yet; do not enable it");

#[cfg(feature = "ringbuf")]
pub mod evt;

use core::alloc::Layout;
use core::mem::MaybeUninit;
use core::ptr;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use async_ringbuf::Arc;
use async_ringbuf::AsyncRb;
use async_ringbuf::traits::{Producer, Split};
use async_ringbuf::wrap::{AsyncCons, AsyncProd};
use ringbuf::SharedRb;
use ringbuf::storage::Ref;

use crate::map::helper::ops::{MapError, MapOps};
use crate::map::helper::{TypedMap, with_typed_map};
use crate::map::runtime::pool::{self, MapKind, RawResource, ResourceId, allocator};

pub const MAX_RECORD_BYTES: usize = 256;
pub const RINGBUF_SLOT_BYTES: usize = MAX_RECORD_BYTES + 4;

/// Fixed-size async ring element (LE u32 length prefix + payload).
pub type RingbufRecord = [u8; RINGBUF_SLOT_BYTES];

/// async-ringbuf consumer half (RX).
pub type RingbufRx = AsyncCons<Arc<AsyncRb<Ref<'static, RingbufRecord>>>>;

/// Parse payload from a [`RingbufRecord`].
pub fn ringbuf_record_payload(record: &RingbufRecord) -> Option<&[u8]> {
    let len = u32::from_le_bytes(record[0..4].try_into().ok()?) as usize;
    if len == 0 || len > MAX_RECORD_BYTES {
        return None;
    }
    Some(&record[4..4 + len])
}

pub fn ringbuf_slot_count(byte_capacity: usize) -> usize {
    core::cmp::max(16, byte_capacity / RINGBUF_SLOT_BYTES)
}

/// Combined BPF TX byte ring + async RX slot ring.
pub struct RingbufMap {
    pub bpf: BpfRingBuf,
    slots: Box<[MaybeUninit<RingbufRecord>], &'static talc::TalcCell<talc::source::Manual>>,
    prod: AsyncProd<Arc<AsyncRb<Ref<'static, RingbufRecord>>>>,
    cons_slot: Option<RingbufRx>,
}

impl RingbufMap {
    pub fn bpf_mut(&mut self) -> &mut BpfRingBuf {
        &mut self.bpf
    }

    pub fn bridge_bpf_to_async(&mut self) {
        let mut scratch = [0u8; MAX_RECORD_BYTES];
        while let Some(len) = self.bpf.try_consume(&mut scratch) {
            let mut record = [0u8; RINGBUF_SLOT_BYTES];
            record[0..4].copy_from_slice(&(len as u32).to_le_bytes());
            record[4..4 + len].copy_from_slice(&scratch[..len]);
            let _ = self.prod.try_push(record);
        }
    }
}

/// BPF byte ring (XDP TX only).
pub struct BpfRingBuf {
    data: Box<[u8], &'static talc::TalcCell<talc::source::Manual>>,
    capacity: usize,
    prod: usize,
    cons: usize,
    reserved: Option<(usize, usize)>,
}

unsafe fn drop_ringbuf_map(ptr: *mut u8) {
    unsafe { pool::reclaim_talc_box::<RingbufMap>(ptr, "RingbufMap") };
}

pub(crate) fn alloc_ringbuf_storage(byte_capacity: usize) -> Result<RawResource, AllocError> {
    if byte_capacity < 16 || byte_capacity > pool::HEAP_SIZE / 2 {
        return Err(AllocError);
    }
    let mut bpf_vec = allocator_api2::vec::Vec::new_in(allocator());
    bpf_vec
        .try_reserve_exact(byte_capacity)
        .map_err(|_| AllocError)?;
    bpf_vec.resize(byte_capacity, 0);
    let bpf_data = bpf_vec.into_boxed_slice();
    let bpf = BpfRingBuf {
        data: bpf_data,
        capacity: byte_capacity,
        prod: 0,
        cons: 0,
        reserved: None,
    };

    let slots = ringbuf_slot_count(byte_capacity);
    let mut slot_vec = allocator_api2::vec::Vec::new_in(allocator());
    slot_vec.try_reserve_exact(slots).map_err(|_| AllocError)?;
    for _ in 0..slots {
        slot_vec.push(MaybeUninit::new([0u8; RINGBUF_SLOT_BYTES]));
    }
    let mut slot_box = slot_vec.into_boxed_slice();
    // SAFETY: `slots` field keeps this buffer alive for the `Ref` inside `Arc<AsyncRb<_>>`.
    let storage = Ref::from(unsafe {
        core::mem::transmute::<
            &mut [MaybeUninit<RingbufRecord>],
            &'static mut [MaybeUninit<RingbufRecord>],
        >(slot_box.as_mut())
    });
    let shared = unsafe { SharedRb::from_raw_parts(storage, 0, 0) };
    let rb = AsyncRb::from(shared);
    let (prod, cons) = rb.split();

    let map = RingbufMap {
        bpf,
        slots: slot_box,
        prod,
        cons_slot: Some(cons),
    };
    let boxed = Box::try_new_in(map, allocator())?;
    let raw = Box::into_raw(boxed) as *mut u8;
    let layout = Layout::new::<RingbufMap>();
    Ok(RawResource::build(
        raw,
        layout,
        drop_ringbuf_map,
        Some(MapKind::Ringbuf),
        0,
        0,
        0,
    ))
}

pub(crate) fn take_rx_consumer(map: &mut RingbufMap) -> Option<RingbufRx> {
    map.cons_slot.take()
}

pub async fn create_bpf_ringbuf(id: ResourceId, byte_capacity: usize) -> Result<(), AllocError> {
    let _ = (id, byte_capacity);
    unimplemented!("RINGBUF is deferred");
}

impl MapOps for BpfRingBuf {
    fn lookup(&self, _key: *const u8) -> Result<*mut u8, MapError> {
        Err(MapError::Unsupported)
    }

    fn update(&mut self, _key: *const u8, _value: *const u8, _flags: u64) -> Result<(), MapError> {
        Err(MapError::Unsupported)
    }

    fn delete(&mut self, _key: *const u8) -> Result<(), MapError> {
        Err(MapError::Unsupported)
    }
}

impl BpfRingBuf {
    const HDR: usize = 4;

    fn free_bytes(&self) -> usize {
        self.capacity.saturating_sub(self.prod)
    }

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
            return -28;
        }
        unsafe { ptr::copy_nonoverlapping(src, dst, size) };
        self.submit();
        0
    }

    pub fn query_avail(&self) -> usize {
        self.prod.saturating_sub(self.cons)
    }

    pub fn try_consume(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.cons + Self::HDR > self.prod {
            return None;
        }
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&self.data[self.cons..self.cons + Self::HDR]);
        let size = u32::from_le_bytes(len_bytes) as usize;
        if size == 0 || size > MAX_RECORD_BYTES {
            return None;
        }
        let payload_off = self.cons + Self::HDR;
        let end = payload_off.checked_add(size)?;
        if end > self.prod || size > out.len() {
            return None;
        }
        out[..size].copy_from_slice(&self.data[payload_off..end]);
        self.cons = end;
        if self.cons == self.prod {
            self.cons = 0;
            self.prod = 0;
        }
        Some(size)
    }

    pub(super) fn reserved_payload_addr(&self) -> Option<usize> {
        let (off, _) = self.reserved?;
        Some(unsafe { self.data.as_ptr().add(off + Self::HDR) as usize })
    }
}

fn with_ring_map<R>(id: ResourceId, f: impl FnOnce(&mut RingbufMap) -> R) -> Option<R> {
    with_typed_map(id, |typed| match typed {
        TypedMap::Ringbuf(rb) => Some(f(rb)),
        _ => None,
    })
    .flatten()
}

fn with_ring_matching<R>(
    data: usize,
    f: impl FnOnce(&mut RingbufMap) -> R,
) -> Option<(ResourceId, R)> {
    let mut guard = pool::pool_try_write()?;
    let mut table = guard.as_mut()?;
    for (id, mut res) in table.iter_mut() {
        if res.kind() != Some(MapKind::Ringbuf) {
            continue;
        }
        let rb: &mut RingbufMap = unsafe { res.as_mut() };
        if rb.bpf.reserved_payload_addr() == Some(data) {
            return Some((id, f(rb)));
        }
    }
    None
}

/// BPF ringbuf helpers are deferred: not dispatched from `helper_dispatcher`.
/// Kept for file structure / future restore; return `-ENOTSUP` if called.
#[allow(dead_code)]
pub fn helper_ringbuf_reserve(map: usize, size: usize, _flags: usize) -> usize {
    let _ = (map, size);
    (-95i32) as usize
}

#[allow(dead_code)]
pub fn helper_ringbuf_submit(data: usize) -> usize {
    let _ = data;
    (-95i32) as usize
}

#[allow(dead_code)]
pub fn helper_ringbuf_discard(data: usize) -> usize {
    let _ = data;
    (-95i32) as usize
}

#[allow(dead_code)]
pub fn helper_ringbuf_output(map: usize, data: usize, size: usize, _flags: usize) -> usize {
    let _ = (map, data, size);
    (-95i32) as usize
}

#[allow(dead_code)]
pub fn helper_ringbuf_query(map: usize, _flags: usize) -> usize {
    let _ = map;
    (-95i32) as usize
}
