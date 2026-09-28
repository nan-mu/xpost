//! Talc allocator + [`RawResource`] storage shell.

use core::{
    alloc::{GlobalAlloc, Layout},
    mem::align_of,
    ptr::NonNull,
};

use allocator_api2::boxed::Box;
use talc::{TalcCell, source::Manual};

use crate::map::MapType;

pub const HEAP_SIZE: usize = 65536;

pub(crate) struct SyncTalc(TalcCell<Manual>);
unsafe impl Sync for SyncTalc {}

impl SyncTalc {
    #[inline]
    pub(crate) fn cell(&'static self) -> &'static TalcCell<Manual> {
        &self.0
    }
}

pub(crate) static ALLOCATOR: SyncTalc = SyncTalc(TalcCell::new(Manual));

#[derive(Clone, Copy)]
pub(crate) struct TalcAlloc;

unsafe impl allocator_api2_v02::alloc::Allocator for TalcAlloc {
    fn allocate(
        &self,
        layout: Layout,
    ) -> Result<NonNull<[u8]>, allocator_api2_v02::alloc::AllocError> {
        if layout.size() == 0 {
            let dangling = unsafe { NonNull::new_unchecked(layout.align() as *mut u8) };
            return Ok(NonNull::slice_from_raw_parts(dangling, 0));
        }
        let ptr = unsafe { GlobalAlloc::alloc(ALLOCATOR.cell(), layout) };
        if ptr.is_null() {
            return Err(allocator_api2_v02::alloc::AllocError);
        }
        Ok(unsafe {
            NonNull::new_unchecked(core::ptr::slice_from_raw_parts_mut(ptr, layout.size()))
        })
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        if layout.size() != 0 {
            unsafe { GlobalAlloc::dealloc(ALLOCATOR.cell(), ptr.as_ptr(), layout) };
        }
    }
}

pub(crate) struct RawResource {
    pub(crate) ptr: *mut u8,
    pub(crate) layout: Layout,
    drop_fn: unsafe fn(*mut u8),
    pub(crate) kind: Option<MapType>,
    pub(crate) users: u32,
}

unsafe impl Send for RawResource {}
unsafe impl Sync for RawResource {}

impl Drop for RawResource {
    fn drop(&mut self) {
        let ptr = core::mem::replace(&mut self.ptr, core::ptr::null_mut());
        if ptr.is_null() {
            return;
        }
        let addr = ptr as usize;
        let align = self.layout.align();
        if !addr.is_multiple_of(align) {
            #[cfg(feature = "perf-counter")]
            crate::ext::perf::counter::inc_pool_drop_misalign();
            return;
        }
        unsafe { (self.drop_fn)(ptr) };
    }
}

pub(crate) unsafe fn reclaim_talc_box<T>(ptr: *mut u8, _tag: &'static str) {
    if ptr.is_null() {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_pool_drop_misalign();
        return;
    }
    let addr = ptr as usize;
    let align = align_of::<T>();
    if !addr.is_multiple_of(align) {
        #[cfg(feature = "perf-counter")]
        crate::ext::perf::counter::inc_pool_drop_misalign();
        return;
    }
    let _ = unsafe { Box::from_raw_in(ptr as *mut T, allocator()) };
}

impl RawResource {
    /// Spec-only placeholder (no heap allocation). Drop is a no-op on null.
    pub(crate) fn unbound() -> Self {
        unsafe fn noop(_: *mut u8) {}
        Self {
            ptr: core::ptr::null_mut(),
            layout: Layout::from_size_align(1, 1).unwrap(),
            drop_fn: noop,
            kind: None,
            users: 0,
        }
    }

    #[inline]
    pub(crate) unsafe fn as_ref<T>(&self) -> &T {
        unsafe { &*(self.ptr as *const T) }
    }

    pub(crate) fn build(
        ptr: *mut u8,
        layout: Layout,
        drop_fn: unsafe fn(*mut u8),
        kind: Option<MapType>,
    ) -> Self {
        Self {
            ptr,
            layout,
            drop_fn,
            kind,
            users: 0,
        }
    }
}

#[inline]
pub(crate) fn allocator() -> &'static TalcCell<Manual> {
    ALLOCATOR.cell()
}

/// Held-lock storage views for BPF helpers (no nested try_lock).
pub(crate) mod access {
    use crate::{
        MapRtGuard,
        map::{MapIdx, MapType, state::MAP_RT},
    };

    pub struct HeapWriteGuard<'a> {
        pub guard: MapRtGuard<'a>,
    }

    pub type HeapReadGuard<'a> = HeapWriteGuard<'a>;

    impl HeapWriteGuard<'_> {
        pub fn get(&self, id: &MapIdx) -> Option<StorageRef> {
            let meta = self.guard.meta.get(*id)?;
            Some(StorageRef {
                kind: meta.storage.kind,
                ptr: meta.storage.ptr as *const u8,
            })
        }

        pub fn get_mut(&mut self, id: &MapIdx) -> Option<StorageMut> {
            let meta = self.guard.meta.get_mut(*id)?;
            Some(StorageMut {
                kind: meta.storage.kind,
                ptr: meta.storage.ptr,
            })
        }
    }

    pub struct StorageRef {
        kind: Option<MapType>,
        ptr: *const u8,
    }

    impl StorageRef {
        pub fn kind(&self) -> Option<MapType> {
            self.kind
        }

        pub unsafe fn as_ref<T>(&self) -> &T {
            unsafe { &*(self.ptr as *const T) }
        }
    }

    pub struct StorageMut {
        kind: Option<MapType>,
        ptr: *mut u8,
    }

    impl StorageMut {
        pub fn kind(&self) -> Option<MapType> {
            self.kind
        }

        pub unsafe fn as_mut<T>(&mut self) -> &mut T {
            unsafe { &mut *(self.ptr as *mut T) }
        }
    }

    #[esp_hal::ram]
    pub fn try_write() -> Option<HeapWriteGuard<'static>> {
        MAP_RT
            .try_get()
            .and_then(|lock| lock.try_lock().ok())
            .map(|guard| HeapWriteGuard { guard })
    }

    #[esp_hal::ram]
    pub fn try_read() -> Option<HeapReadGuard<'static>> {
        try_write()
    }
}

pub(crate) use access::{try_read as heap_try_read, try_write as heap_try_write};
