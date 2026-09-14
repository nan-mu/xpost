//! Map runtime — [`MapRt`], guards, and embedder `init` entrypoint.

use alloc::{string::String, vec::Vec};

use allocator_api2::alloc::AllocError;
use embassy_sync::once_lock::OnceLock;

use crate::map::{
    MapRtGuard, RtMutex,
    builder::{CompleteBuilder, MapType, traits::layout_size},
    kinds::{
        array::{self, BpfArrayMap},
        hash::{self, BpfHashMap, BpfHashTable},
    },
    runtime::pool::{RawResource, allocator},
};
#[cfg(feature = "ringbuf")]
use crate::map::kinds::ringbuf::{self, RingbufRx};

/// Map index into [`MapRt::meta`].
pub type MapIdx = usize;
/// User-facing alias for [`MapIdx`].
pub type MapId = MapIdx;

/// Legacy alias — XDP helpers and codegen still use this name internally.
pub(crate) type ResourceId = MapIdx;

/// Global map runtime singleton.
pub static MAP_RT: OnceLock<RtMutex<MapRt>> = OnceLock::new();

/// Whether talc heap was claimed during [`MapRt::init`].
static mut HEAP_CLAIMED: bool = false;

/// Map runtime and CRUD errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotInitialized,
    AlreadyInitialized,
    HeapClaimFailed,
    NotFound,
    AlreadyExists,
    Busy,
    TypeMismatch,
    Oom,
    InvalidName,
    Unsupported,
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

/// Holds all live map instances.
pub struct MapRt {
    pub(crate) meta: Vec<MapMeta>,
}

/// Per-map metadata row.
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

    /// Claim `heap` for talc, open an empty runtime, return the global lock guard.
    pub async fn init(heap: &'static mut [u8]) -> Result<MapRtGuard<'static>, Error> {
        // SAFETY: single-core MCU; only written during boot before contention.
        if unsafe { HEAP_CLAIMED } {
            return Err(Error::AlreadyInitialized);
        }
        unsafe {
            let base = heap.as_mut_ptr();
            if allocator().claim(base, heap.len()).is_none() {
                return Err(Error::HeapClaimFailed);
            }
            HEAP_CLAIMED = true;
        }
        if MAP_RT.init(RtMutex::new(Self::new())).is_err() {
            return Err(Error::AlreadyInitialized);
        }
        Ok(MAP_RT.get().await.lock().await)
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

    /// Heap budget for a map spec — shared by [`Self::create`] and [`Self::check`].
    pub(crate) fn estimate_map_heap_size(
        map_type: MapType,
        key_layout: Option<&[u8]>,
        value_layout: Option<&[u8]>,
        len: usize,
    ) -> usize {
        match map_type {
            MapType::Hash => {
                let ks = key_layout.map(|l| layout_size(l)).unwrap_or(0);
                let vs = value_layout.map(|l| layout_size(l)).unwrap_or(0);
                len.saturating_mul(ks + vs + 32) + 256
            }
            MapType::Array => {
                let vs = value_layout.map(|l| layout_size(l)).unwrap_or(0);
                let stride = array::BpfArrayMap::value_stride(vs.max(1));
                stride * len + 128
            }
            #[cfg(feature = "ringbuf")]
            MapType::Ringbuf => {
                let slots = ringbuf::ringbuf_slot_count(len);
                len + slots * ringbuf::RINGBUF_SLOT_BYTES + 512
            }
            #[cfg(not(feature = "ringbuf"))]
            MapType::Ringbuf => 0,
        }
    }

    /// Element byte size used in [`crate::xdp::RequiredMap::elem_size`].
    pub(crate) fn map_elem_size(
        map_type: MapType,
        key_layout: Option<&[u8]>,
        value_layout: Option<&[u8]>,
        len: usize,
    ) -> usize {
        match map_type {
            MapType::Hash => {
                key_layout.map(|l| layout_size(l)).unwrap_or(0)
                    + value_layout.map(|l| layout_size(l)).unwrap_or(0)
            }
            MapType::Array => value_layout.map(|l| layout_size(l)).unwrap_or(0),
            MapType::Ringbuf => len,
        }
    }

    /// Assert heap already has a matching, usable map instance for `spec`.
    pub fn check(&self, spec: &CompleteBuilder) -> Result<MapId, Error> {
        let idx = self.find_name(&spec.name).ok_or(Error::NotFound)?;
        let meta = &self.meta[idx];
        if meta.map_type != spec.map_type
            || meta.len != spec.len
            || !Self::layouts_match(
                meta,
                spec.key_layout.as_deref(),
                spec.value_layout.as_deref(),
            )
        {
            return Err(Error::TypeMismatch);
        }
        let expected = Self::estimate_map_heap_size(
            spec.map_type,
            spec.key_layout.as_deref(),
            spec.value_layout.as_deref(),
            spec.len,
        );
        if meta.size != expected {
            return Err(Error::TypeMismatch);
        }
        if meta.storage.ptr.is_null() {
            return Err(Error::TypeMismatch);
        }
        Ok(idx)
    }

    /// Allocate storage and register a new map row.
    pub fn create(&mut self, builder: CompleteBuilder) -> Result<MapId, Error> {
        Self::validate_name(&builder.name)?;
        if self.find_name(&builder.name).is_some() {
            return Err(Error::AlreadyExists);
        }
        let size = Self::estimate_map_heap_size(
            builder.map_type,
            builder.key_layout.as_deref(),
            builder.value_layout.as_deref(),
            builder.len,
        );
        let storage = match builder.map_type {
            MapType::Hash => {
                let kl = builder.key_layout.as_ref().ok_or(Error::TypeMismatch)?;
                let vl = builder.value_layout.as_ref().ok_or(Error::TypeMismatch)?;
                hash::alloc_hash_storage(layout_size(kl), layout_size(vl), builder.len)?
            }
            MapType::Array => {
                let vl = builder.value_layout.as_ref().ok_or(Error::TypeMismatch)?;
                array::alloc_array_storage(layout_size(vl), builder.len)?
            }
            MapType::Ringbuf => {
                #[cfg(feature = "ringbuf")]
                {
                    ringbuf::alloc_ringbuf_storage(builder.len)?
                }
                #[cfg(not(feature = "ringbuf"))]
                {
                    return Err(Error::Unsupported);
                }
            }
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
        let _ = crate::map::runtime::names::register_map_name_idx(&self.meta[idx].name, idx);
        Ok(idx)
    }

    /// Create every non-`runtime` map from a bpf.toml boot descriptor table.
    pub fn create_from_boot_descs(
        &mut self,
        descs: &[crate::map::BootMapDesc],
    ) -> Result<(), Error> {
        for d in descs {
            if d.runtime {
                continue;
            }
            let _ = self.create(CompleteBuilder::from_boot_desc(d))?;
        }
        Ok(())
    }

    pub fn lookup(
        &self,
        name: &str,
        key_layout: Option<&[u8]>,
        value_layout: Option<&[u8]>,
    ) -> Result<MapId, Error> {
        let idx = self.find_name(name).ok_or(Error::NotFound)?;
        if !Self::layouts_match(&self.meta[idx], key_layout, value_layout) {
            return Err(Error::TypeMismatch);
        }
        Ok(idx)
    }

    pub fn lookup_by_key<K: crate::map::MapKey, V: crate::map::MapValue>(
        &self,
        name: &str,
    ) -> Result<MapId, Error> {
        self.lookup(name, Some(K::FIELD_LENS), Some(V::FIELD_LENS))
    }

    pub fn lookup_by_value<V: crate::map::MapValue>(&self, name: &str) -> Result<MapId, Error> {
        self.lookup(name, None, Some(V::FIELD_LENS))
    }

    pub fn find(&self, name: &str) -> Result<MapId, Error> {
        self.find_name(name).ok_or(Error::NotFound)
    }

    pub fn delete(&mut self, idx: MapId) -> Result<(), Error> {
        let Some(meta) = self.meta.get(idx) else {
            return Err(Error::NotFound);
        };
        if meta.storage.users > 0 {
            return Err(Error::Busy);
        }
        crate::map::runtime::names::unregister_map_name(&meta.name);
        self.meta.remove(idx);
        Ok(())
    }

    /// Borrow a map without re-locking [`MAP_RT`].
    pub fn get(&self, idx: MapId) -> Result<MapGuard<'_>, Error> {
        self.meta
            .get(idx)
            .map(|meta| MapGuard { meta })
            .ok_or(Error::NotFound)
    }

    /// Borrow a map mutably without re-locking [`MAP_RT`].
    pub fn get_mut(&mut self, idx: MapId) -> Result<MapMutGuard<'_>, Error> {
        self.meta
            .get_mut(idx)
            .map(|meta| MapMutGuard { meta })
            .ok_or(Error::NotFound)
    }

    pub fn update<F, E>(&mut self, idx: MapId, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut MapMutGuard<'_>) -> Result<(), E>,
        E: Into<Error>,
    {
        let mut guard = self.get_mut(idx)?;
        f(&mut guard).map_err(Into::into)
    }

    pub(crate) fn add_user(&mut self, idx: MapId) -> Result<(), Error> {
        let res = self.meta.get_mut(idx).ok_or(Error::NotFound)?;
        res.storage.users = res.storage.users.saturating_add(1);
        Ok(())
    }

    pub(crate) fn remove_user(&mut self, idx: MapId) -> Result<(), Error> {
        let res = self.meta.get_mut(idx).ok_or(Error::NotFound)?;
        res.storage.users = res.storage.users.saturating_sub(1);
        Ok(())
    }

    pub(crate) fn storage_ptr(&self, idx: MapId) -> Result<usize, Error> {
        let res = self.meta.get(idx).ok_or(Error::NotFound)?;
        Ok(res.storage.ptr as usize)
    }
}

/// [`MapIdx`] extension — ringbuf RX sugar.
#[cfg(feature = "ringbuf")]
pub trait MapIdxExt: Copy {
    fn into_channel(self) -> impl core::future::Future<Output = Result<RingbufRx, Error>>;
}

#[cfg(feature = "ringbuf")]
impl MapIdxExt for MapIdx {
    async fn into_channel(self) -> Result<RingbufRx, Error> {
        let _ = self;
        Err(Error::Unsupported)
    }
}

/// Read-only map access guard (borrows the current [`MapRt`] lock).
pub struct MapGuard<'a> {
    pub(crate) meta: &'a MapMeta,
}

/// Writable map access guard (borrows the current [`MapRt`] lock).
pub struct MapMutGuard<'a> {
    pub(crate) meta: &'a mut MapMeta,
}

impl MapGuard<'_> {
    pub(crate) fn as_hash(&self) -> Result<&BpfHashTable, GuardError> {
        if self.meta.map_type != MapType::Hash {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { self.meta.storage.as_ref::<BpfHashMap>() }.table())
    }

    pub fn as_array(&self) -> Result<&[u8], GuardError> {
        if self.meta.map_type != MapType::Array {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { self.meta.storage.as_ref::<BpfArrayMap>() }.data())
    }

    #[cfg(feature = "ringbuf")]
    pub fn into_channel(self) -> Result<RingbufRx, GuardError> {
        let _ = self;
        Err(GuardError::TypeMismatch)
    }
}

impl MapMutGuard<'_> {
    pub(crate) fn as_hash(&mut self) -> Result<&mut BpfHashTable, GuardError> {
        if self.meta.map_type != MapType::Hash {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { self.meta.storage.as_mut::<BpfHashMap>() }.table_mut())
    }

    pub fn as_array(&mut self) -> Result<&mut [u8], GuardError> {
        if self.meta.map_type != MapType::Array {
            return Err(GuardError::TypeMismatch);
        }
        Ok(unsafe { self.meta.storage.as_mut::<BpfArrayMap>() }.data_mut())
    }
}

// —— crate-internal pool accessors (XDP helpers) ——

pub(crate) fn try_read_storage<R>(idx: MapIdx, f: impl FnOnce(&RawResource) -> R) -> Option<R> {
    let lock = MAP_RT.try_get()?;
    let guard = lock.try_lock().ok()?;
    let res = guard.meta.get(idx)?;
    Some(f(&res.storage))
}

pub(crate) fn try_write_storage<R>(
    idx: MapIdx,
    f: impl FnOnce(&mut RawResource) -> R,
) -> Option<R> {
    let lock = MAP_RT.try_get()?;
    let mut guard = lock.try_lock().ok()?;
    let res = guard.meta.get_mut(idx)?;
    Some(f(&mut res.storage))
}

pub(crate) fn contains(idx: MapIdx) -> bool {
    MAP_RT
        .try_get()
        .and_then(|lock| lock.try_lock().ok())
        .map(|g| idx < g.meta.len())
        .unwrap_or(false)
}

pub(crate) fn remove_user_sync(idx: MapIdx) -> bool {
    let lock = match MAP_RT.try_get() {
        Some(l) => l,
        None => return false,
    };
    let mut guard = match lock.try_lock().ok() {
        Some(g) => g,
        None => return false,
    };
    guard.remove_user(idx).is_ok()
}
