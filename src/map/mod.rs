//! XDP MAP resource pool + BPF map types + helper ABI.
//!
//! Control-plane mutation is serialized by [`MAP_RT`]; helpers resolve AOT
//! VMAs via [`alias`] and operate under a non-nested heap lock.

pub(crate) mod alias;
pub(crate) mod builder;
pub mod dump;
pub(crate) mod heap;
pub(crate) mod kinds;
pub(crate) mod ops;
mod state;

pub use builder::{MapBuilder, MapKey, MapType, MapValue};
pub use heap::HEAP_SIZE;
pub use ops::{BPF_ANY, BPF_EXIST, BPF_NOEXIST};
pub use state::MAP_RT;
pub use xpost_macros::{MapKey, MapValue};

pub use crate::error::{MapCheckError, MapCreateError, MapInitError, MapUseError};
pub use crate::{RtMutex, RtMutexGuard};

/// Locked [`MapRt`].
pub type MapRtGuard<'a> = RtMutexGuard<'a, MapRt>;

/// Map index into [`MapRt`] meta rows.
pub type MapIdx = usize;

pub(crate) use alias::{clear_map_aliases, set_fast_arrays, set_map_aliases};
pub(crate) use ops::{helper_delete, helper_lookup, helper_update};

use alloc::{string::String, vec::Vec};

use crate::map::{
    builder::traits::layout_size,
    heap::{RawResource, allocator},
    kinds::{array, hash},
    ops::MapOps,
    state::HEAP_CLAIMED,
};

/// Holds all live map instances.
pub struct MapRt {
    pub(crate) meta: Vec<MapMeta>,
}

/// Per-map metadata row (also used as XDP required-map dependency specs).
pub struct MapMeta {
    pub name: String,
    pub map_type: MapType,
    pub key_layout: Option<Vec<u8>>,
    pub value_layout: Option<Vec<u8>>,
    pub len: usize,
    pub size: usize,
    pub(crate) storage: RawResource,
}

impl MapMeta {
    /// Build a storage-less spec from a validated [`MapBuilder`].
    pub fn from_builder(builder: MapBuilder) -> Result<Self, MapCreateError> {
        builder.validate()?;
        let name = builder.name.clone().ok_or(MapCreateError::InvalidName)?;
        let size = MapRt::estimate_map_heap_size(
            builder.map_type,
            builder.key_layout.as_deref(),
            builder.value_layout.as_deref(),
            builder.len,
        );
        Ok(Self {
            name,
            map_type: builder.map_type,
            key_layout: builder.key_layout,
            value_layout: builder.value_layout,
            len: builder.len,
            size,
            storage: RawResource::unbound(),
        })
    }

    /// Copy comparison fields; storage is unbound (for XDP required-map rows).
    pub(crate) fn spec_clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            map_type: self.map_type,
            key_layout: self.key_layout.clone(),
            value_layout: self.value_layout.clone(),
            len: self.len,
            size: self.size,
            storage: RawResource::unbound(),
        }
    }

    /// Identity fields only (name matched by caller). `len` is excluded per Compact ELF V0.
    fn identity_eq(&self, other: &MapMeta) -> bool {
        self.map_type == other.map_type
            && self.key_layout == other.key_layout
            && self.value_layout == other.value_layout
    }
}

impl Clone for MapMeta {
    fn clone(&self) -> Self {
        self.spec_clone()
    }
}

impl MapRt {
    fn new() -> Self {
        Self { meta: Vec::new() }
    }

    /// Claim `heap` for talc, open an empty runtime, return the global lock guard.
    pub async fn init(heap: &'static mut [u8]) -> Result<MapRtGuard<'static>, MapInitError> {
        // SAFETY: single-core MCU; only written during boot before contention.
        if unsafe { HEAP_CLAIMED } {
            return Err(MapInitError::AlreadyInitialized);
        }
        unsafe {
            let base = heap.as_mut_ptr();
            if allocator().claim(base, heap.len()).is_none() {
                return Err(MapInitError::HeapClaimFailed);
            }
            HEAP_CLAIMED = true;
        }
        if MAP_RT.init(RtMutex::new(Self::new())).is_err() {
            return Err(MapInitError::AlreadyInitialized);
        }
        Ok(MAP_RT.get().await.lock().await)
    }

    fn find_name(&self, name: &str) -> Option<MapIdx> {
        self.meta.iter().position(|m| m.name == name)
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
                let ks = key_layout.map(layout_size).unwrap_or(0);
                let vs = value_layout.map(layout_size).unwrap_or(0);
                len.saturating_mul(ks + vs + 32) + 256
            }
            MapType::Array => {
                let vs = value_layout.map(layout_size).unwrap_or(0);
                let stride = array::BpfArrayMap::value_stride(vs.max(1));
                stride * len + 128
            }
        }
    }

    /// Assert heap already has a matching, usable map instance for `spec`.
    ///
    /// Identity = (name, map_type, key_layout, value_layout). Existing `len` must
    /// be ≥ requested `len` (no resize); smaller capacity rejects the binding.
    pub fn check(&self, spec: &MapMeta) -> Result<MapIdx, MapCheckError> {
        let idx = self.find_name(&spec.name).ok_or(MapCheckError::NotFound)?;
        let meta = &self.meta[idx];
        if !spec.identity_eq(meta) {
            return Err(MapCheckError::TypeMismatch);
        }
        if meta.len < spec.len {
            return Err(MapCheckError::TypeMismatch);
        }
        if meta.storage.ptr.is_null() {
            return Err(MapCheckError::TypeMismatch);
        }
        Ok(idx)
    }

    /// Allocate storage and register a new map row.
    pub fn create(&mut self, builder: MapBuilder) -> Result<MapIdx, MapCreateError> {
        builder.validate()?;
        let name = builder.name.clone().ok_or(MapCreateError::InvalidName)?;
        if self.find_name(&name).is_some() {
            return Err(MapCreateError::AlreadyExists);
        }
        let size = MapRt::estimate_map_heap_size(
            builder.map_type,
            builder.key_layout.as_deref(),
            builder.value_layout.as_deref(),
            builder.len,
        );
        let storage = match builder.map_type {
            MapType::Hash => {
                let kl = builder
                    .key_layout
                    .as_ref()
                    .ok_or(MapCreateError::TypeMismatch)?;
                let vl = builder
                    .value_layout
                    .as_ref()
                    .ok_or(MapCreateError::TypeMismatch)?;
                hash::alloc_hash_storage(layout_size(kl), layout_size(vl), builder.len)?
            }
            MapType::Array => {
                let vl = builder
                    .value_layout
                    .as_ref()
                    .ok_or(MapCreateError::TypeMismatch)?;
                array::alloc_array_storage(layout_size(vl), builder.len)?
            }
        };
        let idx = self.meta.len();
        self.meta.push(MapMeta {
            name,
            map_type: builder.map_type,
            key_layout: builder.key_layout,
            value_layout: builder.value_layout,
            len: builder.len,
            size,
            storage,
        });
        Ok(idx)
    }

    pub fn find(&self, name: &str) -> Result<MapIdx, MapUseError> {
        self.find_name(name).ok_or(MapUseError::NotFound)
    }

    /// Borrow a map without re-locking [`MAP_RT`].
    pub fn get(&self, idx: MapIdx) -> Result<MapGuard<'_>, MapUseError> {
        self.meta
            .get(idx)
            .map(|meta| MapGuard { meta })
            .ok_or(MapUseError::NotFound)
    }

    /// Control-plane HASH lookup: copy up to `out.len()` value bytes; returns value size.
    pub fn hash_lookup(&self, idx: MapIdx, key: &[u8], out: &mut [u8]) -> Result<usize, MapUseError> {
        let meta = self.meta.get(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_ref::<hash::BpfHashMap>() };
        if key.len() != map.key_size {
            return Err(MapUseError::InvalidArg);
        }
        let p = MapOps::lookup(map, key.as_ptr()).map_err(map_op_to_use)?;
        let n = map.value_size.min(out.len());
        unsafe {
            core::ptr::copy_nonoverlapping(p, out.as_mut_ptr(), n);
        }
        Ok(map.value_size)
    }

    /// Control-plane HASH update (`flags`: [`BPF_ANY`] / [`BPF_NOEXIST`] / [`BPF_EXIST`]).
    pub fn hash_update(
        &mut self,
        idx: MapIdx,
        key: &[u8],
        value: &[u8],
        flags: u64,
    ) -> Result<(), MapUseError> {
        let meta = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_mut::<hash::BpfHashMap>() };
        if key.len() != map.key_size || value.len() != map.value_size {
            return Err(MapUseError::InvalidArg);
        }
        MapOps::update(map, key.as_ptr(), value.as_ptr(), flags).map_err(map_op_to_use)
    }

    /// Control-plane HASH delete.
    pub fn hash_delete(&mut self, idx: MapIdx, key: &[u8]) -> Result<(), MapUseError> {
        let meta = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_mut::<hash::BpfHashMap>() };
        if key.len() != map.key_size {
            return Err(MapUseError::InvalidArg);
        }
        MapOps::delete(map, key.as_ptr()).map_err(map_op_to_use)
    }

    /// Control-plane: remove all HASH entries (capacity unchanged).
    pub fn hash_clear(&mut self, idx: MapIdx) -> Result<(), MapUseError> {
        let meta = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_mut::<hash::BpfHashMap>() };
        map.clear();
        Ok(())
    }

    /// Control-plane: current HASH occupancy.
    pub fn hash_len(&self, idx: MapIdx) -> Result<usize, MapUseError> {
        let meta = self.meta.get(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_ref::<hash::BpfHashMap>() };
        Ok(map.len_used())
    }

    /// Control-plane: HASH value size in bytes.
    pub fn hash_value_size(&self, idx: MapIdx) -> Result<usize, MapUseError> {
        let meta = self.meta.get(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_ref::<hash::BpfHashMap>() };
        Ok(map.value_size)
    }

    /// Control-plane: visit each occupied HASH entry.
    pub fn hash_for_each<F>(&self, idx: MapIdx, f: F) -> Result<(), MapUseError>
    where
        F: FnMut(&[u8], &[u8]),
    {
        let meta = self.meta.get(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Hash {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_ref::<hash::BpfHashMap>() };
        map.for_each(f);
        Ok(())
    }

    /// Control-plane ARRAY update: write `value` as little-endian bytes (truncated/zero-extended
    /// to the map's `value_size`).
    pub fn array_set_u64(
        &mut self,
        idx: MapIdx,
        index: u32,
        value: u64,
    ) -> Result<(), MapUseError> {
        let meta = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Array {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_mut::<array::BpfArrayMap>() };
        let key = index;
        let bytes = value.to_le_bytes();
        let n = map.value_size.min(8);
        let mut buf = [0u8; 8];
        buf[..n].copy_from_slice(&bytes[..n]);
        MapOps::update(
            map,
            &key as *const u32 as *const u8,
            buf.as_ptr(),
            0,
        )
        .map_err(map_op_to_use)
    }

    /// Control-plane: zero every ARRAY element (capacity unchanged).
    pub fn array_clear(&mut self, idx: MapIdx) -> Result<(), MapUseError> {
        let meta = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        if meta.map_type != MapType::Array {
            return Err(MapUseError::TypeMismatch);
        }
        let map = unsafe { meta.storage.as_mut::<array::BpfArrayMap>() };
        map.clear();
        Ok(())
    }

    /// Byte length of the App-mode `get map` ASCII blob for `idx`.
    pub fn map_blob_len(&self, idx: MapIdx) -> Result<usize, MapUseError> {
        let map_type = self.meta.get(idx).ok_or(MapUseError::NotFound)?.map_type;
        match map_type {
            MapType::Array => {
                let mg = self.get(idx)?;
                let size = mg.len() as u32;
                Ok(dump::array_blob_len(size, |i| {
                    mg.array_get_u64(i).unwrap_or(0)
                }))
            }
            MapType::Hash => {
                let cap = self.get(idx)?.len() as u32;
                let size = self.hash_len(idx)? as u32;
                let mut len = dump::hash_header_len(size, cap);
                self.hash_for_each(idx, |key, value| {
                    len += dump::hash_entry_len(key, value);
                })?;
                Ok(len)
            }
        }
    }

    /// Stream the App-mode `get map` ASCII blob for `idx` into `write`.
    pub fn write_map_blob(
        &self,
        idx: MapIdx,
        mut write: impl FnMut(&[u8]),
    ) -> Result<(), MapUseError> {
        let map_type = self.meta.get(idx).ok_or(MapUseError::NotFound)?.map_type;
        match map_type {
            MapType::Array => {
                let mg = self.get(idx)?;
                let size = mg.len() as u32;
                dump::write_array_blob(size, |i| mg.array_get_u64(i).unwrap_or(0), &mut write);
                Ok(())
            }
            MapType::Hash => {
                let cap = self.get(idx)?.len() as u32;
                let size = self.hash_len(idx)? as u32;
                dump::write_hash_header(size, cap, &mut write);
                self.hash_for_each(idx, |key, value| {
                    dump::write_hash_entry(key, value, &mut write);
                })
            }
        }
    }

    pub(crate) fn add_user(&mut self, idx: MapIdx) -> Result<(), MapUseError> {
        let res = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        res.storage.users = res.storage.users.saturating_add(1);
        Ok(())
    }

    pub(crate) fn remove_user(&mut self, idx: MapIdx) -> Result<(), MapUseError> {
        let res = self.meta.get_mut(idx).ok_or(MapUseError::NotFound)?;
        res.storage.users = res.storage.users.saturating_sub(1);
        Ok(())
    }
}

fn map_op_to_use(e: ops::MapOpError) -> MapUseError {
    match e {
        ops::MapOpError::NotFound => MapUseError::NotFound,
        ops::MapOpError::InvalidArg | ops::MapOpError::Unsupported => MapUseError::InvalidArg,
        ops::MapOpError::Exists => MapUseError::Exists,
        ops::MapOpError::NoMem => MapUseError::NoMem,
        ops::MapOpError::Busy => MapUseError::Busy,
    }
}

/// Read-only map access guard (borrows the current [`MapRt`] lock).
pub struct MapGuard<'a> {
    pub(crate) meta: &'a MapMeta,
}

impl MapGuard<'_> {
    pub fn name(&self) -> &str {
        &self.meta.name
    }

    pub fn map_type(&self) -> MapType {
        self.meta.map_type
    }

    /// ARRAY entry count (`max_entries`).
    pub fn len(&self) -> usize {
        self.meta.len
    }

    /// Control-plane: read ARRAY element `index` as little-endian u64 (zero-extend).
    pub fn array_get_u64(&self, index: u32) -> Result<u64, MapUseError> {
        if self.meta.map_type != MapType::Array {
            return Err(MapUseError::TypeMismatch);
        }
        let arr = unsafe { self.meta.storage.as_ref::<array::BpfArrayMap>() };
        let key = index;
        let p = MapOps::lookup(arr, &key as *const u32 as *const u8).map_err(|e| match e {
            crate::map::ops::MapOpError::NotFound => MapUseError::NotFound,
            _ => MapUseError::TypeMismatch,
        })?;
        let vs = arr.value_size.min(8);
        let mut buf = [0u8; 8];
        unsafe {
            core::ptr::copy_nonoverlapping(p, buf.as_mut_ptr(), vs);
        }
        Ok(u64::from_le_bytes(buf))
    }
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
