//! Sync BPF map helper ABI (AOT `map` arg → index → concrete ops).
//!
//! # Safety subset (ARRAY / HASH)
//!
//! - **ARRAY**: lookup / update; delete → Unsupported (Linux-aligned).
//! - **HASH**: lookup / update / delete; flags `BPF_ANY|NOEXIST|EXIST`.
//! - **lookup null**: miss and pool Busy both return `0` (indistinguishable).
//! - **value pointer**: valid for the rest of the current flight while the map
//!   remains leased; delete (HASH) invalidates prior lookup pointers.

use crate::map::{
    MapIdx, MapType,
    alias::resolve_map_id,
    heap::{heap_try_read, heap_try_write},
    kinds::{array::BpfArrayMap, hash::BpfHashMap},
};

/// Linux `bpf_map_update_elem` flags (low bits).
pub(crate) const BPF_ANY: u64 = 0;
pub(crate) const BPF_NOEXIST: u64 = 1;
pub(crate) const BPF_EXIST: u64 = 2;

/// BPF map operation failure (Linux-aligned errno via [`MapOpError::errno`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapOpError {
    InvalidArg,
    NotFound,
    Exists,
    NoMem,
    Unsupported,
    Busy,
}

impl MapOpError {
    /// Soft-fail on the helper path: bump [`crate::xdp::counter`] for bug-class errors.
    /// `NotFound` / `Exists` stay quiet (normal datapath misses).
    pub(crate) fn count_if_bug(self) {
        match self {
            Self::NotFound | Self::Exists => {}
            Self::Busy | Self::InvalidArg | Self::NoMem | Self::Unsupported => {
                crate::xdp::counter::inc_helper_map();
            }
        }
    }

    pub(crate) fn errno(self) -> i32 {
        match self {
            Self::InvalidArg | Self::Unsupported => -22, // EINVAL
            Self::NotFound => -2,                        // ENOENT
            Self::Exists => -17,                         // EEXIST
            Self::NoMem => -12,                          // ENOMEM
            Self::Busy => -11,                           // EAGAIN
        }
    }
}

/// Pointer ABI used by AOT helpers and firmware seed/control paths.
pub(crate) trait MapOps {
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapOpError>;
    fn update(&mut self, key: *const u8, value: *const u8, flags: u64) -> Result<(), MapOpError>;
    fn delete(&mut self, key: *const u8) -> Result<(), MapOpError>;
}

enum TypedMap<'a> {
    Array(&'a mut BpfArrayMap),
    Hash(&'a mut BpfHashMap),
}

enum TypedMapRef<'a> {
    Array(&'a BpfArrayMap),
    Hash(&'a BpfHashMap),
}

fn with_typed_map<R>(id: MapIdx, f: impl FnOnce(TypedMap<'_>) -> R) -> Option<R> {
    let mut guard = heap_try_write()?;
    let mut res = guard.get_mut(&id)?;
    let typed = match res.kind()? {
        MapType::Array => TypedMap::Array(unsafe { res.as_mut() }),
        MapType::Hash => TypedMap::Hash(unsafe { res.as_mut() }),
    };
    Some(f(typed))
}

fn with_typed_map_ref<R>(id: MapIdx, f: impl FnOnce(TypedMapRef<'_>) -> R) -> Option<R> {
    let guard = heap_try_read()?;
    let res = guard.get(&id)?;
    let typed = match res.kind()? {
        MapType::Array => TypedMapRef::Array(unsafe { res.as_ref() }),
        MapType::Hash => TypedMapRef::Hash(unsafe { res.as_ref() }),
    };
    Some(f(typed))
}

/// Sync BPF helper entry: resolve `map` as AOT VMA alias only (fail-closed).
#[esp_hal::ram]
pub(crate) fn helper_lookup(map: usize, key: usize) -> usize {
    if let Some(p) = crate::map::alias::fast_array_lookup(map, key) {
        return p;
    }
    let Some(id) = resolve_map_id(map) else {
        crate::xdp::counter::inc_helper_unresolved();
        return 0;
    };
    let key = key as *const u8;
    match with_typed_map_ref(id, |typed| match typed {
        TypedMapRef::Array(m) => MapOps::lookup(m, key),
        TypedMapRef::Hash(m) => MapOps::lookup(m, key),
    }) {
        Some(Ok(p)) => p as usize,
        Some(Err(MapOpError::NotFound)) => 0,
        Some(Err(MapOpError::Unsupported)) => {
            MapOpError::Unsupported.count_if_bug();
            0
        }
        Some(Err(_)) => 0,
        None => {
            MapOpError::Busy.count_if_bug();
            0
        }
    }
}

pub(crate) fn helper_update(map: usize, key: usize, value: usize, flags: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return MapOpError::InvalidArg.errno() as usize;
    };
    let key = key as *const u8;
    let value = value as *const u8;
    match with_typed_map(id, |typed| match typed {
        TypedMap::Array(m) => MapOps::update(m, key, value, flags as u64),
        TypedMap::Hash(m) => MapOps::update(m, key, value, flags as u64),
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            e.count_if_bug();
            e.errno() as usize
        }
        None => {
            MapOpError::Busy.count_if_bug();
            MapOpError::Busy.errno() as usize
        }
    }
}

pub(crate) fn helper_delete(map: usize, key: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return MapOpError::InvalidArg.errno() as usize;
    };
    let key = key as *const u8;
    match with_typed_map(id, |typed| match typed {
        TypedMap::Array(m) => MapOps::delete(m, key),
        TypedMap::Hash(m) => MapOps::delete(m, key),
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            e.count_if_bug();
            e.errno() as usize
        }
        None => {
            MapOpError::Busy.count_if_bug();
            MapOpError::Busy.errno() as usize
        }
    }
}
