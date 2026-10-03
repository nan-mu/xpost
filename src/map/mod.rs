//! XDP MAP resource pool + BPF map types + helper ABI.
//!
//! Control-plane mutation is serialized by [`MAP_RT`]; helpers resolve AOT
//! VMAs via [`alias`] and operate under a non-nested heap lock.

mod alias;
pub(crate) mod builder;
pub(crate) mod heap;
pub(crate) mod kinds;
pub(crate) mod ops;
mod state;

pub use builder::{MapBuilder, MapKey, MapType, MapValue};
pub use heap::HEAP_SIZE;
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
                let kl = builder.key_layout.as_ref().ok_or(MapCreateError::TypeMismatch)?;
                let vl = builder.value_layout.as_ref().ok_or(MapCreateError::TypeMismatch)?;
                hash::alloc_hash_storage(layout_size(kl), layout_size(vl), builder.len)?
            }
            MapType::Array => {
                let vl = builder.value_layout.as_ref().ok_or(MapCreateError::TypeMismatch)?;
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
