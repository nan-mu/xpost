//! Talc-backed resource pool (Id → [`RawResource`]).
//!
//! Named **pool** (not “register”) to avoid clashing with MCU MMIO registers.
//! Locking lives in [`crate::patch::sync::RwCell`] (`patch-sync-embassy` /
//! `patch-sync-st`).

use core::alloc::{GlobalAlloc, Layout};
use core::mem::align_of;
use core::ptr::NonNull;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use hashbrown::HashMap;
use log::error;
use talc::TalcCell;
use talc::source::Manual;

use crate::patch::sync::RwCell;

pub(crate) const HEAP_SIZE: usize = 65536;

#[unsafe(link_section = ".bss")]
#[unsafe(no_mangle)]
static mut XDP_MAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// Wrapper that allows placing `TalcCell` in a `static`.
///
/// # Safety
/// Safe on single-core MCUs (ESP32-C3) where all async tasks run on one
/// executor and interrupt-level code never calls into the allocator.
pub(crate) struct SyncTalc(TalcCell<Manual>);
unsafe impl Sync for SyncTalc {}

impl SyncTalc {
    #[inline]
    pub(crate) fn cell(&'static self) -> &'static TalcCell<Manual> {
        &self.0
    }

    #[inline]
    #[cfg_attr(not(feature = "hw-selftest"), allow(dead_code))]
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

/// Bridge `hashbrown`'s allocator-api2 0.2 `Allocator` trait to talc via
/// `GlobalAlloc`.
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

/// Numeric handle for a pooled resource.
pub type ResourceId = u32;

/// Concrete BPF map kind stored at insert time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapKind {
    Array,
    Hash,
    Ringbuf,
    /// `BPF_MAP_TYPE_DEVMAP`: key `u32` → value `u32` ifindex (ARRAY layout).
    Devmap,
}

type PoolMap = HashMap<ResourceId, RawResource, hashbrown::DefaultHashBuilder, TalcAlloc>;

/// Global Id → resource table. Sync XDP helpers use [`RwCell::try_write`].
pub(crate) static POOL: RwCell<Option<PoolMap>> = RwCell::new(None);

/// Type-erased pool entry. [`Drop`] returns storage to talc — no manual free at
/// call sites.
pub struct RawResource {
    pub ptr: *mut u8,
    pub layout: Layout,
    drop_fn: unsafe fn(*mut u8),
    /// `None` = non-BPF pool object (hwtest probes, …).
    pub kind: Option<MapKind>,
    pub key_size: usize,
    pub value_size: usize,
    pub value_stride: usize,
    /// XDP programs currently holding this map (blocks destroy while > 0).
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
                "[pool] drop: ptr {:#x} misaligned (layout align={}, size={}, kind={:?}); leaking",
                addr,
                align,
                self.layout.size(),
                self.kind
            );
            return;
        }
        // SAFETY: ptr + drop_fn paired at insert; align checked against stored layout.
        unsafe { (self.drop_fn)(ptr) };
    }
}

/// Drop a `Box<T, TalcAlloc>` previously produced by [`Box::into_raw`] + [`allocator`].
///
/// On null / misalignment: logs and **leaks** instead of calling `from_raw` (avoids UB).
///
/// # Safety
/// When checks pass, `ptr` must be the unique live pointer from `Box::into_raw` for `T`
/// allocated with [`allocator`].
pub(crate) unsafe fn reclaim_talc_box<T>(ptr: *mut u8, tag: &'static str) {
    if ptr.is_null() {
        error!("[pool] reclaim {}: null pointer; skip", tag);
        return;
    }
    let addr = ptr as usize;
    let align = align_of::<T>();
    if addr % align != 0 {
        error!(
            "[pool] reclaim {}: ptr {:#x} not aligned to {} (wrong type/drop_fn?); leaking",
            tag, addr, align
        );
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

    fn build(
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

    fn insert_into(table: &mut PoolMap, id: ResourceId, resource: Self) -> Result<(), AllocError> {
        if table.contains_key(&id) {
            return Err(AllocError);
        }
        table.insert(id, resource);
        Ok(())
    }

    /// Insert into the pool. On Id collision the new entry is dropped (talc freed).
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
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
        let mut guard = POOL.write().await;
        let table = guard.as_mut().expect("pool not initialized");
        Self::insert_into(table, id, resource)
    }

    /// Insert into the pool. On Id collision the new entry is dropped (talc freed).
    #[cfg(feature = "patch-sync-st")]
    pub(crate) fn insert(
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
        let mut guard = POOL.write();
        let table = guard.as_mut().expect("pool not initialized");
        Self::insert_into(table, id, resource)
    }

    /// Read a pooled resource under the read lock.
    ///
    /// # Safety contract
    /// Caller must ensure `T` matches the type stored under `id`.
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn read<T, R>(id: ResourceId, f: impl FnOnce(&T) -> R) -> Option<R> {
        let guard = POOL.read().await;
        let table = guard.as_ref()?;
        let resource = table.get(&id)?;
        Some(f(unsafe { resource.as_ref() }))
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn read<T, R>(id: ResourceId, f: impl FnOnce(&T) -> R) -> Option<R> {
        let guard = POOL.read();
        let table = guard.as_ref()?;
        let resource = table.get(&id)?;
        Some(f(unsafe { resource.as_ref() }))
    }

    /// Mutably access a pooled resource under the write lock.
    ///
    /// # Safety contract
    /// Caller must ensure `T` matches the type stored under `id`.
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn with_mut<T, R>(id: ResourceId, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = POOL.write().await;
        let table = guard.as_mut()?;
        let resource = table.get_mut(&id)?;
        Some(f(unsafe { resource.as_mut() }))
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn with_mut<T, R>(id: ResourceId, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut guard = POOL.write();
        let table = guard.as_mut()?;
        let resource = table.get_mut(&id)?;
        Some(f(unsafe { resource.as_mut() }))
    }

    /// Remove a resource; [`Drop`] returns memory to talc.
    ///
    /// Returns `false` if missing. Does **not** check [`Self::users`] — callers
    /// that honor XDP refcounts must use [`crate::patch::map::ctrl::destroy_map`].
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn destroy(id: ResourceId) -> bool {
        let mut guard = POOL.write().await;
        let table = match guard.as_mut() {
            Some(t) => t,
            None => return false,
        };
        table.remove(&id).is_some()
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn destroy(id: ResourceId) -> bool {
        let mut guard = POOL.write();
        let table = match guard.as_mut() {
            Some(t) => t,
            None => return false,
        };
        table.remove(&id).is_some()
    }

    /// Whether `id` is present in the pool.
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn contains(id: ResourceId) -> bool {
        let guard = POOL.read().await;
        guard.as_ref().is_some_and(|t| t.contains_key(&id))
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn contains(id: ResourceId) -> bool {
        let guard = POOL.read();
        guard.as_ref().is_some_and(|t| t.contains_key(&id))
    }

    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn users(id: ResourceId) -> Option<u32> {
        let guard = POOL.read().await;
        Some(guard.as_ref()?.get(&id)?.users)
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn users(id: ResourceId) -> Option<u32> {
        let guard = POOL.read();
        Some(guard.as_ref()?.get(&id)?.users)
    }

    /// Increment program refcount. Returns `false` if map missing.
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn add_user(id: ResourceId) -> bool {
        let mut guard = POOL.write().await;
        let res = match guard.as_mut().and_then(|t| t.get_mut(&id)) {
            Some(r) => r,
            None => return false,
        };
        res.users = res.users.saturating_add(1);
        true
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn add_user(id: ResourceId) -> bool {
        let mut guard = POOL.write();
        let res = match guard.as_mut().and_then(|t| t.get_mut(&id)) {
            Some(r) => r,
            None => return false,
        };
        res.users = res.users.saturating_add(1);
        true
    }

    /// Decrement program refcount. Returns `false` if map missing.
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn remove_user(id: ResourceId) -> bool {
        let mut guard = POOL.write().await;
        let res = match guard.as_mut().and_then(|t| t.get_mut(&id)) {
            Some(r) => r,
            None => return false,
        };
        res.users = res.users.saturating_sub(1);
        true
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn remove_user(id: ResourceId) -> bool {
        let mut guard = POOL.write();
        let res = match guard.as_mut().and_then(|t| t.get_mut(&id)) {
            Some(r) => r,
            None => return false,
        };
        res.users = res.users.saturating_sub(1);
        true
    }

    /// Allocate the lowest free Id in `0..max_id`.
    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    pub async fn alloc_id(max_id: ResourceId) -> Option<ResourceId> {
        let guard = POOL.read().await;
        let table = guard.as_ref()?;
        (0..max_id).find(|id| !table.contains_key(id))
    }

    #[cfg(feature = "patch-sync-st")]
    pub fn alloc_id(max_id: ResourceId) -> Option<ResourceId> {
        let guard = POOL.read();
        let table = guard.as_ref()?;
        (0..max_id).find(|id| !table.contains_key(id))
    }
}

/// Return a `&'static` reference to the talc allocator cell.
#[inline]
pub fn allocator() -> &'static TalcCell<Manual> {
    ALLOCATOR.cell()
}

/// Claim `XDP_MAP` and open an empty pool table.
pub fn init() {
    unsafe {
        let base = (&raw mut XDP_MAP).cast::<u8>();
        ALLOCATOR
            .cell()
            .claim(base, HEAP_SIZE)
            .expect("[Init][MAP] talc: failed to claim XDP_MAP");
    }

    let map = HashMap::with_capacity_in(16, TalcAlloc);
    POOL.try_write()
        .expect("[Init][MAP] pool init before contention")
        .replace(map);
}
