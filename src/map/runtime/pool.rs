//! Talc allocator + [`RawResource`] storage shell.

use core::{
    alloc::{GlobalAlloc, Layout},
    mem::align_of,
    ptr::NonNull,
};

use allocator_api2::{alloc::AllocError, boxed::Box};
use talc::{TalcCell, source::Manual};

use crate::map::runtime::rt::{self as runtime, MapIdx};

pub const HEAP_SIZE: usize = 65536;

pub(crate) struct SyncTalc(TalcCell<Manual>);
unsafe impl Sync for SyncTalc {}

impl SyncTalc {
    #[inline]
    pub(crate) fn cell(&'static self) -> &'static TalcCell<Manual> {
        &self.0
    }

    #[inline]
    pub(crate) fn allocated_bytes(&self) -> usize {
        self.0.counters().allocated_bytes
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn available_bytes(&self) -> usize {
        self.0.counters().available_bytes
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn claimed_bytes(&self) -> usize {
        self.0.counters().claimed_bytes
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

pub(crate) type ResourceId = MapIdx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MapKind {
    Array,
    Hash,
    #[cfg(feature = "ringbuf")]
    Ringbuf,
}

pub(crate) struct RawResource {
    pub(crate) ptr: *mut u8,
    pub(crate) layout: Layout,
    drop_fn: unsafe fn(*mut u8),
    pub(crate) kind: Option<MapKind>,
    pub(crate) key_size: usize,
    pub(crate) value_size: usize,
    pub(crate) value_stride: usize,
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
        if addr % align != 0 {
            crate::xdp::counter::inc_pool_drop_misalign();
            return;
        }
        unsafe { (self.drop_fn)(ptr) };
    }
}

pub(crate) unsafe fn reclaim_talc_box<T>(ptr: *mut u8, _tag: &'static str) {
    if ptr.is_null() {
        crate::xdp::counter::inc_pool_drop_misalign();
        return;
    }
    let addr = ptr as usize;
    let align = align_of::<T>();
    if addr % align != 0 {
        crate::xdp::counter::inc_pool_drop_misalign();
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
            key_size: 0,
            value_size: 0,
            value_stride: 0,
            users: 0,
        }
    }

    #[inline]
    pub(crate) unsafe fn as_ref<T>(&self) -> &T {
        unsafe { &*(self.ptr as *const T) }
    }

    #[inline]
    pub(crate) unsafe fn as_mut<T>(&mut self) -> &mut T {
        unsafe { &mut *(self.ptr as *mut T) }
    }

    pub(crate) fn build(
        ptr: *mut u8,
        layout: Layout,
        drop_fn: unsafe fn(*mut u8),
        kind: Option<MapKind>,
        key_size: usize,
        value_size: usize,
        value_stride: usize,
    ) -> Self {
        Self {
            ptr,
            layout,
            drop_fn,
            kind,
            key_size,
            value_size,
            value_stride,
            users: 0,
        }
    }

    #[cfg(feature = "ringbuf")]
    pub(crate) fn as_ringbuf_map_mut(&mut self) -> &mut crate::map::kinds::ringbuf::RingbufMap {
        unsafe { self.as_mut() }
    }

    pub(crate) async fn insert(
        id: ResourceId,
        ptr: *mut u8,
        layout: Layout,
        drop_fn: unsafe fn(*mut u8),
        kind: Option<MapKind>,
        key_size: usize,
        value_size: usize,
        value_stride: usize,
    ) -> Result<(), AllocError> {
        let resource = Self::build(
            ptr,
            layout,
            drop_fn,
            kind,
            key_size,
            value_size,
            value_stride,
        );
        let mut guard = runtime::MAP_RT.get().await.lock().await;
        if guard.meta.len() != id {
            return Err(AllocError);
        }
        guard.meta.push(runtime::MapMeta {
            name: alloc::format!("map_{id}"),
            map_type: crate::map::builder::MapType::Hash,
            key_layout: None,
            value_layout: None,
            len: 0,
            size: 0,
            storage: resource,
        });
        Ok(())
    }

    pub(crate) async fn destroy(id: ResourceId) -> bool {
        let lock = match runtime::MAP_RT.try_get() {
            Some(l) => l,
            None => return false,
        };
        let mut guard = match lock.try_lock().ok() {
            Some(g) => g,
            None => return false,
        };
        if id >= guard.meta.len() {
            return false;
        }
        guard.meta.remove(id);
        true
    }
}

#[inline]
pub(crate) fn allocator() -> &'static TalcCell<Manual> {
    ALLOCATOR.cell()
}

pub(crate) mod pool_access {
    use crate::map::{
        MapRtGuard,
        runtime::{
            pool::{MapKind, ResourceId},
            rt as runtime,
        },
    };

    pub struct PoolWriteGuard<'a> {
        /// Held for the duration of helper / pool access (lock token).
        pub guard: MapRtGuard<'a>,
    }

    /// Shared with write path — Mutex has a single guard type.
    pub type PoolReadGuard<'a> = PoolWriteGuard<'a>;

    impl PoolWriteGuard<'_> {
        /// Snapshot storage fields under the held lock (no nested try_lock).
        pub fn get(&self, id: &ResourceId) -> Option<StorageRef> {
            let meta = self.guard.meta.get(*id)?;
            Some(StorageRef {
                id: *id,
                kind: meta.storage.kind,
                ptr: meta.storage.ptr as *const u8,
            })
        }

        pub fn get_mut(&mut self, id: &ResourceId) -> Option<StorageMut> {
            let meta = self.guard.meta.get_mut(*id)?;
            Some(StorageMut {
                id: *id,
                kind: meta.storage.kind,
                ptr: meta.storage.ptr,
                value_size: meta.storage.value_size,
                value_stride: meta.storage.value_stride,
            })
        }

        /// Iterate all map slots as storage snapshots (held-lock, no nested try_lock).
        pub fn iter_mut(&mut self) -> impl Iterator<Item = (ResourceId, StorageMut)> + '_ {
            self.guard.meta.iter_mut().enumerate().map(|(id, meta)| {
                (
                    id,
                    StorageMut {
                        id,
                        kind: meta.storage.kind,
                        ptr: meta.storage.ptr,
                        value_size: meta.storage.value_size,
                        value_stride: meta.storage.value_stride,
                    },
                )
            })
        }

        /// Compat: former `as_ref()` / `as_mut()` returned a shim; now the guard
        /// itself is the table view.
        pub fn as_ref(&self) -> Option<&Self> {
            Some(self)
        }

        pub fn as_mut(&mut self) -> Option<&mut Self> {
            Some(self)
        }
    }

    pub struct StorageRef {
        pub id: ResourceId,
        kind: Option<MapKind>,
        ptr: *const u8,
    }

    impl StorageRef {
        pub fn kind(&self) -> Option<MapKind> {
            self.kind
        }

        pub unsafe fn as_ref<T>(&self) -> &T {
            unsafe { &*(self.ptr as *const T) }
        }
    }

    pub struct StorageMut {
        pub id: ResourceId,
        kind: Option<MapKind>,
        ptr: *mut u8,
        value_size: usize,
        value_stride: usize,
    }

    impl StorageMut {
        pub fn kind(&self) -> Option<MapKind> {
            self.kind
        }

        pub fn value_size(&self) -> usize {
            self.value_size
        }

        pub fn value_stride(&self) -> usize {
            self.value_stride
        }

        pub unsafe fn as_mut<T>(&mut self) -> &mut T {
            unsafe { &mut *(self.ptr as *mut T) }
        }
    }

    pub fn try_write() -> Option<PoolWriteGuard<'static>> {
        runtime::MAP_RT
            .try_get()
            .and_then(|lock| lock.try_lock().ok())
            .map(|guard| PoolWriteGuard { guard })
    }

    pub fn try_read() -> Option<PoolReadGuard<'static>> {
        try_write()
    }
}

pub(crate) use pool_access::{try_read as pool_try_read, try_write as pool_try_write};
