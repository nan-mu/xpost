//! Map runtime — [`MapRt`], guards, and embedder `claim_heap` / `init` entrypoints.

use alloc::string::String;
use alloc::vec::Vec;

use allocator_api2::alloc::AllocError;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::rwlock::{RwLockReadGuard, RwLockWriteGuard};

use crate::sync::RwCell;

use super::array::{self, BpfArrayMap};
use super::builder::{CompleteBuilder, MapType};
use super::hash::{self, BpfHashMap, BpfHashTable};
use super::pool::{self, MapKind, RawResource, allocator};
use super::ringbuf::{self, RingbufMap};
use super::traits::{layout_size, layout_vec};
use super::wdt_feed;

/// Map index into [`MapRt::meta`].
pub type MapIdx = usize;

/// Legacy alias — XDP helpers and codegen still use this name internally.
pub type ResourceId = MapIdx;

/// Global map runtime singleton.
pub static MAP_RT: RwCell<Option<MapRt>> = RwCell::new(None);

/// Whether talc heap was claimed via [`claim_heap`].
static mut HEAP_CLAIMED: bool = false;

/// Map runtime and CRUD errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotInitialized,
    NotFound,
    AlreadyExists,
    Busy,
    TypeMismatch,
    Oom,
    InvalidName,
}

impl From<AllocError> for Error {
    fn from(_: AllocError) -> Self {
        Self::Oom
    }
}

/// [`MapGuard`] / [`MapMutGuard`] conversion errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardError {
    TypeMismatch,
}

impl From<GuardError> for Error {
    fn from(e: GuardError) -> Self {
        match e {
            GuardError::TypeMismatch => Self::TypeMismatch,
        }
    }
}

/// Register embedder-provided SRAM as the map talc heap. Call once before [`init`].
pub fn claim_heap(heap: &'static mut [u8]) -> Result<(), Error> {
    // SAFETY: single-core MCU; only written during boot before contention.
    if unsafe { HEAP_CLAIMED } {
        return Err(Error::Busy);
    }
    unsafe {
        let base = heap.as_mut_ptr();
        allocator()
            .claim(base, heap.len())
            .ok_or(Error::Oom)?;
        HEAP_CLAIMED = true;
    }
    Ok(())
}

/// Open an empty [`MapRt`]. Requires prior [`claim_heap`].
pub async fn init() -> Result<(), Error> {
    if !unsafe { HEAP_CLAIMED } {
        return Err(Error::NotInitialized);
    }
    let mut guard = MAP_RT.write().await;
    if guard.is_some() {
        return Err(Error::Busy);
    }
    guard.replace(MapRt::new());
    Ok(())
}

/// Exclusive handle to the global [`MapRt`].
pub async fn map_rt<'a>() -> Result<MapRtWriteGuard<'a>, Error> {
    let guard = MAP_RT.write().await;
    if guard.is_none() {
        return Err(Error::NotInitialized);
    }
    Ok(MapRtWriteGuard { guard })
}

/// Read handle to the global [`MapRt`].
pub async fn map_rt_read<'a>() -> Result<MapRtReadGuard<'a>, Error> {
    let guard = MAP_RT.read().await;
    if guard.is_none() {
        return Err(Error::NotInitialized);
    }
    Ok(MapRtReadGuard { guard })
}

/// Write guard dereferencing to [`MapRt`].
pub struct MapRtWriteGuard<'a> {
    guard: RwLockWriteGuard<'a, CriticalSectionRawMutex, Option<MapRt>>,
}

impl core::ops::Deref for MapRtWriteGuard<'_> {
    type Target = MapRt;

    fn deref(&self) -> &Self::Target {
        self.guard.as_ref().expect("map_rt guard")
    }
}

impl core::ops::DerefMut for MapRtWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.as_mut().expect("map_rt guard")
    }
}

/// Read guard dereferencing to [`MapRt`].
pub struct MapRtReadGuard<'a> {
    guard: RwLockReadGuard<'a, CriticalSectionRawMutex, Option<MapRt>>,
}

impl core::ops::Deref for MapRtReadGuard<'_> {
    type Target = MapRt;

    fn deref(&self) -> &Self::Target {
        self.guard.as_ref().expect("map_rt guard")
    }
}

/// Holds all live map instances.
pub struct MapRt {
    pub(crate) meta: Vec<MapMeta>,
}

/// Per-map metadata row (user-visible fields per map-runtime-plan §2.1.1).
pub struct MapMeta {
    pub name: String,
    pub map_type: MapType,
    pub key_layout: Option<Vec<u8>>,
    pub value_layout: Option<Vec<u8>>,
    pub len: usize,
    pub size: usize,
    pub(crate) storage: RawResource,
}

impl MapRt {
    fn new() -> Self {
        Self { meta: Vec::new() }
    }

    fn validate_name(name: &str) -> Result<(), Error> {
        if name.is_empty() || name.len() > 15 || !name.is_ascii() {
            return Err(Error::InvalidName);
        }
        Ok(())
    }

    fn find_name(&self, name: &str) -> Option<MapIdx> {
        self.meta.iter().position(|m| m.name == name)
    }

    fn layouts_match(meta: &MapMeta, key: Option<&[u8]>, value: Option<&[u8]>) -> bool {
        meta.key_layout.as_deref() == key && meta.value_layout.as_deref() == value
    }

    fn estimate_size(
        map_type: MapType,
        key_layout: Option<&[u8]>,
        value_layout: Option<&[u8]>,
        len: usize,
    ) -> usize {
        match map_type {
            MapType::Hash => {
                let ks = key_layout.map(|l| layout_size(l)).unwrap_or(0);
                let vs = value_layout.map(|l| layout_size(l)).unwrap_or(0);
                // hashbrown nodes + BpfHashMap shell (conservative).
                len.saturating_mul(ks + vs + 32) + 256
            }
            MapType::Array => {
                let vs = value_layout.map(|l| layout_size(l)).unwrap_or(0);
                let stride = array::BpfArrayMap::value_stride(vs.max(1));
                stride * len + 128
            }
            MapType::Ringbuf => {
                let slots = ringbuf::ringbuf_slot_count(len);
                len + slots * ringbuf::RINGBUF_SLOT_BYTES + 512
            }
        }
    }

    /// Allocate storage and register a new map row.
    pub async fn push(&mut self, builder: CompleteBuilder) -> Result<MapIdx, Error> {
        Self::validate_name(&builder.name)?;
        if self.find_name(&builder.name).is_some() {
            return Err(Error::AlreadyExists);
        }
        let size = Self::estimate_size(
            builder.map_type,
            builder.key_layout.as_deref(),
            builder.value_layout.as_deref(),
            builder.len,
        );
        let storage = match builder.map_type {
            MapType::Hash => {
                let kl = builder.key_layout.as_ref().ok_or(Error::TypeMismatch)?;
                let vl = builder.value_layout.as_ref().ok_or(Error::TypeMismatch)?;
                let key_size = layout_size(kl);
                let value_size = layout_size(vl);
                hash::alloc_hash_storage(key_size, value_size, builder.len)?
            }
            MapType::Array => {
                let vl = builder.value_layout.as_ref().ok_or(Error::TypeMismatch)?;
                let value_size = layout_size(vl);
                array::alloc_array_storage(value_size, builder.len)?
            }
            MapType::Ringbuf => ringbuf::alloc_ringbuf_storage(builder.len)?,
        };
        let idx = self.meta.len();
        self.meta.push(MapMeta {
            name: builder.name,
            map_type: builder.map_type,
            key_layout: builder.key_layout,
            value_layout: builder.value_layout,
            len: builder.len,
            size,
            storage,
        });
        let _ = super::names::register_map_name_idx(&self.meta[idx].name, idx);
        Ok(idx)
    }

    pub async fn lookup(
        &self,
        name: &str,
        key_layout: Option<&[u8]>,
        value_layout: Option<&[u8]>,
    ) -> Result<MapIdx, Error> {
        let idx = self.find_name(name).ok_or(Error::NotFound)?;
        if !Self::layouts_match(&self.meta[idx], key_layout, value_layout) {
            return Err(Error::TypeMismatch);
        }
        Ok(idx)
    }

    pub async fn lookup_by_key<K: super::MapKey, V: super::MapValue>(
        &self,
        name: &str,
    ) -> Result<MapIdx, Error> {
        self.lookup(name, Some(K::FIELD_LENS), Some(V::FIELD_LENS))
            .await
    }

    pub async fn lookup_by_value<V: super::MapValue>(&self, name: &str) -> Result<MapIdx, Error> {
        self.lookup(name, None, Some(V::FIELD_LENS)).await
    }

    pub async fn find(&self, name: &str) -> Result<MapIdx, Error> {
        self.find_name(name).ok_or(Error::NotFound)
    }

    pub async fn delete(&mut self, idx: MapIdx) -> Result<(), Error> {
        let meta = self.meta.get(idx).ok_or(Error::NotFound)?;
        if meta.storage.users > 0 {
            return Err(Error::Busy);
        }
        super::names::unregister_map_name(&meta.name);
        self.meta.remove(idx);
        Ok(())
    }

    pub async fn get(&self, idx: MapIdx) -> Result<MapGuard<'_>, Error> {
        if idx >= self.meta.len() {
            return Err(Error::NotFound);
        }
        let guard = MAP_RT.read().await;
        if guard.is_none() {
            return Err(Error::NotInitialized);
        }
        Ok(MapGuard { _guard: guard, idx })
    }

    pub async fn get_mut(&mut self, idx: MapIdx) -> Result<MapMutGuard<'_>, Error> {
        if idx >= self.meta.len() {
            return Err(Error::NotFound);
        }
        let guard = MAP_RT.write().await;
        if guard.is_none() {
            return Err(Error::NotInitialized);
        }
        Ok(MapMutGuard { _guard: guard, idx })
    }

    pub async fn update<F, E>(&mut self, idx: MapIdx, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut MapMutGuard<'_>) -> Result<(), E>,
        E: Into<Error>,
    {
        let mut guard = self.get_mut(idx).await?;
        f(&mut guard).map_err(Into::into)
    }
}

/// [`MapIdx`] extension — ringbuf RX sugar.
pub trait MapIdxExt: Copy {
    fn into_channel(self) -> impl core::future::Future<Output = Result<ringbuf::RingbufRx, Error>>;
}

impl MapIdxExt for MapIdx {
    async fn into_channel(self) -> Result<ringbuf::RingbufRx, Error> {
        let mut guard = MAP_RT.write().await;
        let rt = guard.as_mut().ok_or(Error::NotInitialized)?;
        let meta = rt.meta.get_mut(self).ok_or(Error::NotFound)?;
        if meta.map_type != MapType::Ringbuf {
            return Err(Error::TypeMismatch);
        }
        ringbuf::take_rx_consumer(meta.storage.as_ringbuf_map_mut()).ok_or(Error::Busy)
    }
}

/// Read-only map access guard (holds [`MAP_RT`] read lock).
pub struct MapGuard<'a> {
    pub(crate) _guard: RwLockReadGuard<'a, CriticalSectionRawMutex, Option<MapRt>>,
    pub(crate) idx: MapIdx,
}

/// Writable map access guard (holds [`MAP_RT`] write lock).
pub struct MapMutGuard<'a> {
    pub(crate) _guard: RwLockWriteGuard<'a, CriticalSectionRawMutex, Option<MapRt>>,
    pub(crate) idx: MapIdx,
}

impl MapGuard<'_> {
    fn meta(&self) -> Result<&MapMeta, GuardError> {
        self._guard
            .as_ref()
            .and_then(|rt| rt.meta.get(self.idx))
            .ok_or(GuardError::TypeMismatch)
    }

    pub fn as_hash(
        &self,
    ) -> Result<&BpfHashTable, GuardError> {
        let meta = self.meta()?;
        if meta.map_type != MapType::Hash {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { meta.storage.as_ref::<BpfHashMap>() }.table())
    }

    pub fn as_array(&self) -> Result<&[u8], GuardError> {
        let meta = self.meta()?;
        if meta.map_type != MapType::Array {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { meta.storage.as_ref::<BpfArrayMap>() }.data())
    }

    pub fn into_channel(self) -> Result<ringbuf::RingbufRx, GuardError> {
        let idx = self.idx;
        let map_type = self.meta()?.map_type;
        if map_type != MapType::Ringbuf {
            return Err(GuardError::TypeMismatch);
        }
        drop(self);
        let mut guard = MAP_RT.try_write().ok_or(GuardError::TypeMismatch)?;
        let rt = guard.as_mut().ok_or(GuardError::TypeMismatch)?;
        let meta = rt.meta.get_mut(idx).ok_or(GuardError::TypeMismatch)?;
        ringbuf::take_rx_consumer(meta.storage.as_ringbuf_map_mut()).ok_or(GuardError::TypeMismatch)
    }
}

impl MapMutGuard<'_> {
    fn meta_mut(&mut self) -> Result<&mut MapMeta, GuardError> {
        self._guard
            .as_mut()
            .and_then(|rt| rt.meta.get_mut(self.idx))
            .ok_or(GuardError::TypeMismatch)
    }

    pub fn as_hash(
        &mut self,
    ) -> Result<&mut BpfHashTable, GuardError> {
        let meta = self.meta_mut()?;
        if meta.map_type != MapType::Hash {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { meta.storage.as_mut::<BpfHashMap>() }.table_mut())
    }

    pub fn as_array(&mut self) -> Result<&mut [u8], GuardError> {
        let meta = self.meta_mut()?;
        if meta.map_type != MapType::Array {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { meta.storage.as_mut::<BpfArrayMap>() }.data_mut())
    }
}

// —— crate-internal pool accessors (XDP helpers) ——

pub(crate) fn try_read_storage<R>(idx: MapIdx, f: impl FnOnce(&RawResource) -> R) -> Option<R> {
    let guard = MAP_RT.try_read()?;
    let rt = guard.as_ref()?;
    let res = rt.meta.get(idx)?;
    Some(f(&res.storage))
}

pub(crate) fn try_write_storage<R>(idx: MapIdx, f: impl FnOnce(&mut RawResource) -> R) -> Option<R> {
    let mut guard = MAP_RT.try_write()?;
    let rt = guard.as_mut()?;
    let res = rt.meta.get_mut(idx)?;
    Some(f(&mut res.storage))
}

pub(crate) fn try_write_rt<R>(f: impl FnOnce(&mut MapRt) -> R) -> Option<R> {
    let mut guard = MAP_RT.try_write()?;
    let rt = guard.as_mut()?;
    Some(f(rt))
}

pub(crate) fn contains(idx: MapIdx) -> bool {
    MAP_RT
        .try_read()
        .and_then(|g| g.as_ref().map(|rt| idx < rt.meta.len()))
        .unwrap_or(false)
}

pub(crate) async fn users(idx: MapIdx) -> Option<u32> {
    let guard = MAP_RT.read().await;
    Some(guard.as_ref()?.meta.get(idx)?.storage.users)
}

pub(crate) async fn add_user(idx: MapIdx) -> bool {
    let mut guard = MAP_RT.write().await;
    let rt = match guard.as_mut() {
        Some(r) => r,
        None => return false,
    };
    let res = match rt.meta.get_mut(idx) {
        Some(r) => r,
        None => return false,
    };
    res.storage.users = res.storage.users.saturating_add(1);
    true
}

pub(crate) fn add_user_sync(idx: MapIdx) -> bool {
    let mut guard = match MAP_RT.try_write() {
        Some(g) => g,
        None => return false,
    };
    let rt = match guard.as_mut() {
        Some(r) => r,
        None => return false,
    };
    let res = match rt.meta.get_mut(idx) {
        Some(r) => r,
        None => return false,
    };
    res.storage.users = res.storage.users.saturating_add(1);
    true
}

pub(crate) async fn remove_user(idx: MapIdx) -> bool {
    let mut guard = MAP_RT.write().await;
    let rt = match guard.as_mut() {
        Some(r) => r,
        None => return false,
    };
    let res = match rt.meta.get_mut(idx) {
        Some(r) => r,
        None => return false,
    };
    res.storage.users = res.storage.users.saturating_sub(1);
    true
}

pub(crate) fn meta_kind(idx: MapIdx) -> Option<MapKind> {
    try_read_storage(idx, |s| s.kind).flatten()
}

/// Boot helper: create wdt feed map at fixed idx (codegen path).
pub(crate) async fn push_wdt_feed_at(
    idx: MapIdx,
    name: &str,
    max_entries: usize,
) -> Result<(), Error> {
    let mut guard = MAP_RT.write().await;
    let rt = guard.as_mut().ok_or(Error::NotInitialized)?;
    while rt.meta.len() < idx {
        // Pad for deterministic codegen indices.
        return Err(Error::NotFound);
    }
    if rt.meta.len() > idx {
        return Err(Error::AlreadyExists);
    }
    let storage = wdt_feed::alloc_wdt_feed_storage(max_entries)?;
    rt.meta.push(MapMeta {
        name: String::from(name),
        map_type: MapType::Array, // placeholder — kind in storage
        key_layout: None,
        value_layout: None,
        len: max_entries,
        size: max_entries * 8,
        storage,
    });
    let _ = super::names::register_map_name_idx(name, idx);
    Ok(())
}

/// Boot helper: create devmap / prog_array at idx.
pub(crate) async fn push_devmap_at(idx: MapIdx, name: &str, max_entries: usize) -> Result<(), Error> {
    push_special_array_at(idx, name, max_entries, MapKind::Devmap).await
}

pub(crate) async fn push_prog_array_at(
    idx: MapIdx,
    name: &str,
    max_entries: usize,
) -> Result<(), Error> {
    push_special_array_at(idx, name, max_entries, MapKind::ProgArray).await
}

async fn push_special_array_at(
    idx: MapIdx,
    name: &str,
    max_entries: usize,
    kind: MapKind,
) -> Result<(), Error> {
    let mut guard = MAP_RT.write().await;
    let rt = guard.as_mut().ok_or(Error::NotInitialized)?;
    if rt.meta.len() != idx {
        return Err(if rt.meta.len() > idx {
            Error::AlreadyExists
        } else {
            Error::NotFound
        });
    }
    let storage = array::alloc_devmap_storage(max_entries, kind)?;
    rt.meta.push(MapMeta {
        name: String::from(name),
        map_type: MapType::Array,
        key_layout: None,
        value_layout: Some(alloc::vec![4]),
        len: max_entries,
        size: max_entries * 4,
        storage,
    });
    let _ = super::names::register_map_name_idx(name, idx);
    Ok(())
}

/// Legacy synchronous init for embedders not yet on async boot.
pub fn init_sync() {
    if !unsafe { HEAP_CLAIMED } {
        panic!("[Init][MAP] claim_heap required before init");
    }
    MAP_RT
        .try_write()
        .expect("[Init][MAP] pool init before contention")
        .replace(MapRt::new());
}
