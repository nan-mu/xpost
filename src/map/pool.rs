//! Talc allocator + [`RawResource`] storage shell.

use core::alloc::{GlobalAlloc, Layout};
use core::mem::align_of;
use core::ptr::NonNull;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use hashbrown::HashMap;
use log::error;
use talc::TalcCell;
use talc::source::Manual;

use super::runtime::{self, MapIdx, MapRt};

pub const HEAP_SIZE: usize = 65536;

pub struct SyncTalc(TalcCell<Manual>);
unsafe impl Sync for SyncTalc {}

impl SyncTalc {
    #[inline]
    pub(crate) fn cell(&'static self) -> &'static TalcCell<Manual> {
        &self.0
    }

    #[inline]
    pub fn allocated_bytes(&self) -> usize {
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

pub static ALLOCATOR: SyncTalc = SyncTalc(TalcCell::new(Manual));

#[derive(Clone, Copy)]
pub struct TalcAlloc;

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

pub type ResourceId = MapIdx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    Array,
    Hash,
    Ringbuf,
    Devmap,
    ProgArray,
    WdtFeed,
}

pub struct RawResource {
    pub ptr: *mut u8,
    pub layout: Layout,
    drop_fn: unsafe fn(*mut u8),
    pub kind: Option<MapKind>,
    pub key_size: usize,
    pub value_size: usize,
    pub value_stride: usize,
    pub users: u32,
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
            error!(
                "[pool] drop: ptr {:#x} misaligned; leaking",
                addr
            );
            return;
        }
        unsafe { (self.drop_fn)(ptr) };
    }
}

pub unsafe fn reclaim_talc_box<T>(ptr: *mut u8, tag: &'static str) {
    if ptr.is_null() {
        error!("[pool] reclaim {}: null", tag);
        return;
    }
    let addr = ptr as usize;
    let align = align_of::<T>();
    if addr % align != 0 {
        error!("[pool] reclaim {}: misaligned; leaking", tag);
        return;
    }
    let _ = unsafe { Box::from_raw_in(ptr as *mut T, allocator()) };
}

impl RawResource {
    #[inline]
    pub unsafe fn as_ref<T>(&self) -> &T {
        unsafe { &*(self.ptr as *const T) }
    }

    #[inline]
    pub unsafe fn as_mut<T>(&mut self) -> &mut T {
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

    pub(crate) fn as_ringbuf_map_mut(&mut self) -> &mut super::ringbuf::RingbufMap {
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
        let mut guard = runtime::MAP_RT.write().await;
        let rt = guard.as_mut().ok_or(AllocError)?;
        if rt.meta.len() != id {
            return Err(AllocError);
        }
        rt.meta.push(super::runtime::MapMeta {
            name: alloc::format!("map_{id}"),
            map_type: super::builder::MapType::Hash,
            key_layout: None,
            value_layout: None,
            len: 0,
            size: 0,
            storage: resource,
        });
        Ok(())
    }

    pub(crate) async fn read<T, R>(id: ResourceId, f: impl FnOnce(&T) -> R) -> Option<R> {
        runtime::try_read_storage(id, |res| f(unsafe { res.as_ref() }))
    }

    pub(crate) async fn with_mut<T, R>(id: ResourceId, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        runtime::try_write_storage(id, |res| f(unsafe { res.as_mut() }))
    }

    pub(crate) async fn destroy(id: ResourceId) -> bool {
        let mut guard = match runtime::MAP_RT.try_write() {
            Some(g) => g,
            None => return false,
        };
        let rt = match guard.as_mut() {
            Some(r) => r,
            None => return false,
        };
        if id >= rt.meta.len() {
            return false;
        }
        rt.meta.remove(id);
        true
    }

    pub(crate) async fn contains(id: ResourceId) -> bool {
        runtime::contains(id)
    }

    pub(crate) async fn users(id: ResourceId) -> Option<u32> {
        runtime::users(id).await
    }

    pub(crate) async fn add_user(id: ResourceId) -> bool {
        runtime::add_user(id).await
    }

    pub(crate) fn add_user_sync(id: ResourceId) -> bool {
        runtime::add_user_sync(id)
    }

    pub(crate) async fn remove_user(id: ResourceId) -> bool {
        runtime::remove_user(id).await
    }

    pub(crate) async fn alloc_id(max_id: ResourceId) -> Option<ResourceId> {
        let guard = runtime::MAP_RT.read().await;
        let rt = guard.as_ref()?;
        if rt.meta.len() < max_id {
            Some(rt.meta.len())
        } else {
            None
        }
    }
}

#[inline]
pub fn allocator() -> &'static TalcCell<Manual> {
    ALLOCATOR.cell()
}

#[deprecated(note = "use claim_heap + init")]
pub fn init() {
    super::runtime::init_sync();
}

type PoolMap = HashMap<ResourceId, RawResource, hashbrown::DefaultHashBuilder, TalcAlloc>;

pub(crate) mod pool_access {
    use super::*;

    pub struct PoolWriteGuard<'a> {
        pub guard: embassy_sync::rwlock::RwLockWriteGuard<
            'a,
            embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
            Option<MapRt>,
        >,
    }

    #[derive(Clone, Copy)]
    pub struct PoolMapShim;

    impl PoolWriteGuard<'_> {
        pub fn as_mut(&mut self) -> Option<PoolMapShim> {
            self.guard.is_some().then_some(PoolMapShim)
        }
    }

    pub struct PoolReadGuard<'a> {
        pub guard: embassy_sync::rwlock::RwLockReadGuard<
            'a,
            embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex,
            Option<MapRt>,
        >,
    }

    impl PoolReadGuard<'_> {
        pub fn as_ref(&self) -> Option<PoolMapShim> {
            self.guard.is_some().then_some(PoolMapShim)
        }
    }

    impl PoolMapShim {
        pub fn get(&self, id: &ResourceId) -> Option<StorageRef> {
            if runtime::contains(*id) {
                Some(StorageRef { id: *id })
            } else {
                None
            }
        }

        pub fn get_mut(&mut self, id: &ResourceId) -> Option<StorageMut> {
            if runtime::contains(*id) {
                Some(StorageMut { id: *id })
            } else {
                None
            }
        }

        pub fn contains_key(&self, id: &ResourceId) -> bool {
            runtime::contains(*id)
        }

        pub fn iter_mut(&mut self) -> PoolIterMut {
            let len = runtime::MAP_RT
                .try_read()
                .and_then(|g| g.as_ref().map(|rt| rt.meta.len()))
                .unwrap_or(0);
            PoolIterMut { next: 0, len }
        }
    }

    pub struct StorageRef {
        pub id: ResourceId,
    }

    impl StorageRef {
        pub fn kind(&self) -> Option<MapKind> {
            runtime::try_read_storage(self.id, |r| r.kind).flatten()
        }

        pub fn value_size(&self) -> usize {
            runtime::try_read_storage(self.id, |r| r.value_size).unwrap_or(0)
        }

        pub fn value_stride(&self) -> usize {
            runtime::try_read_storage(self.id, |r| r.value_stride).unwrap_or(0)
        }

        pub unsafe fn as_ref<T>(&self) -> &T {
            let ptr = runtime::try_read_storage(self.id, |r| r.ptr as *const T)
                .expect("storage");
            unsafe { &*ptr }
        }

        pub unsafe fn as_mut<T>(&mut self) -> &mut T {
            let ptr = runtime::try_write_storage(self.id, |r| r.ptr as *mut T)
                .expect("storage");
            unsafe { &mut *ptr }
        }
    }

    pub struct StorageMut {
        pub id: ResourceId,
    }

    impl StorageMut {
        pub fn kind(&self) -> Option<MapKind> {
            runtime::try_read_storage(self.id, |r| r.kind).flatten()
        }

        pub fn value_size(&self) -> usize {
            runtime::try_read_storage(self.id, |r| r.value_size).unwrap_or(0)
        }

        pub fn value_stride(&self) -> usize {
            runtime::try_read_storage(self.id, |r| r.value_stride).unwrap_or(0)
        }

        pub unsafe fn as_ref<T>(&self) -> &T {
            let ptr = runtime::try_read_storage(self.id, |r| r.ptr as *const T)
                .expect("storage");
            unsafe { &*ptr }
        }

        pub unsafe fn as_mut<T>(&mut self) -> &mut T {
            let ptr = runtime::try_write_storage(self.id, |r| r.ptr as *mut T)
                .expect("storage");
            unsafe { &mut *ptr }
        }
    }

    pub struct PoolIterMut {
        next: usize,
        len: usize,
    }

    impl Iterator for PoolIterMut {
        type Item = (ResourceId, StorageMut);

        fn next(&mut self) -> Option<Self::Item> {
            if self.next >= self.len {
                return None;
            }
            let id = self.next;
            self.next += 1;
            Some((id, StorageMut { id }))
        }
    }

    pub fn try_write() -> Option<PoolWriteGuard<'static>> {
        runtime::MAP_RT.try_write().map(|guard| PoolWriteGuard { guard })
    }

    pub fn try_read() -> Option<PoolReadGuard<'static>> {
        runtime::MAP_RT.try_read().map(|guard| PoolReadGuard { guard })
    }
}

pub(crate) use pool_access::{try_read as pool_try_read, try_write as pool_try_write};

pub(crate) static POOL: PoolShim = PoolShim;

pub(crate) struct PoolShim;

impl PoolShim {
    pub fn try_read(&self) -> Option<pool_access::PoolReadGuard<'static>> {
        pool_access::try_read()
    }

    pub fn try_write(&self) -> Option<pool_access::PoolWriteGuard<'static>> {
        pool_access::try_write()
    }
}

pub(crate) use runtime::MAP_RT;
