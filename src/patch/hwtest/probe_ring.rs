//! Talc-pool probe: typed `SharedRb` (not BPF `BpfRingBuf`).

use core::alloc::Layout;
use core::mem::MaybeUninit;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use ringbuf::SharedRb;
use ringbuf::ring_buffer::Container;
use talc::TalcCell;
use talc::source::Manual;

use crate::patch::map::pool::{RawResource, ResourceId, allocator, reclaim_talc_box};

/// Backing storage for [`ProbeRingBuf`], allocated from talc.
pub struct ProbeRingStorage<T>(allocator_api2::vec::Vec<MaybeUninit<T>, &'static TalcCell<Manual>>);

unsafe impl<T> Container<T> for ProbeRingStorage<T> {
    type Internal = Self;

    fn into_internal(self) -> Self {
        self
    }

    unsafe fn from_internal(this: Self) -> Self {
        this
    }

    fn as_mut_ptr(this: &Self) -> *mut MaybeUninit<T> {
        this.0.as_ptr() as *mut _
    }

    fn len(this: &Self) -> usize {
        this.0.len()
    }
}

/// Heap-backed ring used only by pool self-checks.
pub type ProbeRingBuf<T> = SharedRb<T, ProbeRingStorage<T>>;

unsafe fn drop_probe_ringbuf<T>(ptr: *mut u8) {
    unsafe { reclaim_talc_box::<ProbeRingBuf<T>>(ptr, "ProbeRingBuf") };
}

fn alloc_probe_ringbuf<T: Send + 'static>(
    capacity: usize,
) -> Result<(*mut u8, Layout), AllocError> {
    if capacity == 0 {
        return Err(AllocError);
    }
    let alloc = allocator();
    let mut data = allocator_api2::vec::Vec::new_in(alloc);
    data.try_reserve_exact(capacity).map_err(|_| AllocError)?;
    data.resize_with(capacity, MaybeUninit::<T>::uninit);
    let rb = unsafe { SharedRb::from_raw_parts(ProbeRingStorage(data), 0, 0) };
    let boxed = Box::try_new_in(rb, alloc)?;
    let raw_ptr = Box::into_raw(boxed) as *mut u8;
    let layout = Layout::new::<ProbeRingBuf<T>>();
    Ok((raw_ptr, layout))
}

/// Insert a fixed-capacity probe ring (`kind = None`).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn create_probe_ringbuf<T: Send + 'static>(
    id: ResourceId,
    capacity: usize,
) -> Result<(), AllocError> {
    let (raw_ptr, layout) = alloc_probe_ringbuf::<T>(capacity)?;
    RawResource::insert(id, raw_ptr, layout, drop_probe_ringbuf::<T>, None, 0, 0, 0).await
}

#[cfg(feature = "patch-sync-st")]
pub fn create_probe_ringbuf<T: Send + 'static>(
    id: ResourceId,
    capacity: usize,
) -> Result<(), AllocError> {
    let (raw_ptr, layout) = alloc_probe_ringbuf::<T>(capacity)?;
    RawResource::insert(id, raw_ptr, layout, drop_probe_ringbuf::<T>, None, 0, 0, 0)
}
