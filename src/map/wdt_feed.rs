//! `BPF_MAP_TYPE_XPOST_WDT_FEED` — per-channel idle stamps for XDP feed + ISR expiry.
//!
//! Value layout matches [`super::wdt_types::WdtFeedSlot`]. BPF map helpers on
//! this kind are not in the ARRAY/HASH allowlist; `bpf_xpost_wdt_feed` is not
//! dispatched. Firmware idle stamps belong to feature `event-ringbuf`.

#![allow(unsafe_op_in_unsafe_fn)]

use core::ptr;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;

use super::ops::{MapError, MapOps};
use super::pool::{HEAP_SIZE, MapKind, RawResource, ResourceId, allocator};
use super::wdt_types::{FeedHandle, WdtFeedSlot, MAX_CHANNELS};

const SLOT_SIZE: usize = core::mem::size_of::<WdtFeedSlot>();

/// XPOST watchdog feed map (ARRAY semantics, custom idle integration).
pub struct BpfWdtFeedMap {
    max_entries: usize,
    data: Box<[u8], &'static talc::TalcCell<talc::source::Manual>>,
}

impl BpfWdtFeedMap {
    #[inline]
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    #[inline]
    pub fn slot(&self, idx: usize) -> Option<&WdtFeedSlot> {
        if idx >= self.max_entries {
            return None;
        }
        // SAFETY: slots are `WdtFeedSlot`-aligned in `data`.
        Some(unsafe { &*(self.data.as_ptr().add(idx * SLOT_SIZE) as *const WdtFeedSlot) })
    }

    #[inline]
    pub fn slot_mut(&mut self, idx: usize) -> Option<&mut WdtFeedSlot> {
        if idx >= self.max_entries {
            return None;
        }
        Some(unsafe { &mut *(self.data.as_mut_ptr().add(idx * SLOT_SIZE) as *mut WdtFeedSlot) })
    }
}

impl MapOps for BpfWdtFeedMap {
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapError> {
        if key.is_null() {
            return Err(MapError::InvalidArg);
        }
        let idx = unsafe { ptr::read_unaligned(key as *const u32) } as usize;
        let slot = self.slot(idx).ok_or(MapError::NotFound)?;
        Ok(slot as *const WdtFeedSlot as *mut u8)
    }

    fn update(&mut self, key: *const u8, value: *const u8, _flags: u64) -> Result<(), MapError> {
        if key.is_null() {
            return Err(MapError::InvalidArg);
        }
        let idx = unsafe { ptr::read_unaligned(key as *const u32) } as usize;
        let slot = self.slot_mut(idx).ok_or(MapError::NotFound)?;
        if !value.is_null() {
            let incoming = unsafe { ptr::read_unaligned(value as *const WdtFeedSlot) };
            // BPF may only refresh `last_feed_us`; preserve firmware `idle_us` / `armed`.
            if incoming.last_feed_us != 0 {
                slot.last_feed_us = incoming.last_feed_us;
            }
        } else {
            slot.last_feed_us = embassy_time::Instant::now().as_micros();
        }
        Ok(())
    }

    fn delete(&mut self, _key: *const u8) -> Result<(), MapError> {
        Err(MapError::Unsupported)
    }
}

fn alloc_wdt_feed(max_entries: usize) -> Result<(*mut u8, core::alloc::Layout), AllocError> {
    if max_entries == 0 || max_entries > MAX_CHANNELS {
        return Err(AllocError);
    }
    let total = SLOT_SIZE
        .checked_mul(max_entries)
        .ok_or(AllocError)?;
    if total > HEAP_SIZE {
        return Err(AllocError);
    }
    let mut vec = allocator_api2::vec::Vec::new_in(allocator());
    vec.try_reserve_exact(total).map_err(|_| AllocError)?;
    vec.resize(total, 0);
    let data = vec.into_boxed_slice();
    let map = BpfWdtFeedMap { max_entries, data };
    let boxed = Box::try_new_in(map, allocator())?;
    let raw = Box::into_raw(boxed) as *mut u8;
    let layout = core::alloc::Layout::new::<BpfWdtFeedMap>();
    Ok((raw, layout))
}

unsafe fn drop_wdt_feed(ptr: *mut u8) {
    unsafe { super::pool::reclaim_talc_box::<BpfWdtFeedMap>(ptr, "BpfWdtFeedMap") };
}

/// Create `wdt_feed` map and bind it to [`watchdog::bind_feed_map`].
pub async fn create_bpf_wdt_feed(id: ResourceId, max_entries: usize) -> Result<(), AllocError> {
    let storage = alloc_wdt_feed_storage(max_entries)?;
    RawResource::insert(
        id,
        storage.ptr,
        storage.layout,
        drop_wdt_feed,
        Some(MapKind::WdtFeed),
        core::mem::size_of::<u32>(),
        SLOT_SIZE,
        SLOT_SIZE,
    )
    .await?;
    #[cfg(feature = "event-ringbuf")]
    super::ringbuf::watchdog::bind_feed_map(id);
    Ok(())
}

pub(crate) fn alloc_wdt_feed_storage(max_entries: usize) -> Result<RawResource, AllocError> {
    let (raw, layout) = alloc_wdt_feed(max_entries)?;
    Ok(RawResource::build(
        raw,
        layout,
        drop_wdt_feed,
        Some(MapKind::WdtFeed),
        core::mem::size_of::<u32>(),
        SLOT_SIZE,
        SLOT_SIZE,
    ))
}

/// BPF helper leftover (`bpf_xpost_wdt_feed`); dispatcher does not route here.
pub fn helper_feed(handle: usize) -> usize {
    let _ = FeedHandle(handle as u32);
    usize::MAX
}
