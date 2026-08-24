//! BPF_MAP_TYPE_HASH — byte-keyed hash table in the talc pool.

#![allow(unsafe_op_in_unsafe_fn)]

use core::alloc::Layout;
use core::hash::{Hash, Hasher};
use core::mem::size_of;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use hashbrown::HashMap;
use log::error;

use super::abi::{BPF_EXIST, BPF_NOEXIST};
use super::names::register_map_name;
use super::ops::{MapError, MapOps};
use super::pool::{MapKind, RawResource, ResourceId, TalcAlloc, allocator};

const MAX_KEY_BYTES: usize = 64;
const MAX_VAL_BYTES: usize = 256;

/// Inner hashbrown table of [`BpfHashMap`] (`KeyBlob` → `ValSlot`).
pub type BpfHashTable = HashMap<KeyBlob, ValSlot, hashbrown::DefaultHashBuilder, TalcAlloc>;

/// Opaque key bytes for [`BpfHashMap`].
#[derive(Clone, Copy)]
pub struct KeyBlob {
    len: u8,
    data: [u8; MAX_KEY_BYTES],
}

impl PartialEq for KeyBlob {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.data[..self.len as usize] == other.data[..other.len as usize]
    }
}
impl Eq for KeyBlob {}
impl Hash for KeyBlob {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.data[..self.len as usize].hash(state);
    }
}

impl KeyBlob {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_KEY_BYTES {
            return None;
        }
        let mut data = [0u8; MAX_KEY_BYTES];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            len: bytes.len() as u8,
            data,
        })
    }

    unsafe fn from_ptr(p: *const u8, len: usize) -> Option<Self> {
        if p.is_null() || len == 0 || len > MAX_KEY_BYTES {
            return None;
        }
        let bytes = unsafe { core::slice::from_raw_parts(p, len) };
        Self::from_bytes(bytes)
    }
}

/// Opaque value slot for [`BpfHashMap`].
pub struct ValSlot {
    #[allow(dead_code)]
    len: usize,
    data: [u8; MAX_VAL_BYTES],
}

impl ValSlot {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_VAL_BYTES {
            return None;
        }
        let mut data = [0u8; MAX_VAL_BYTES];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Self {
            len: bytes.len(),
            data,
        })
    }

    unsafe fn from_ptr(p: *const u8, len: usize) -> Option<Self> {
        if p.is_null() || len == 0 || len > MAX_VAL_BYTES {
            return None;
        }
        let bytes = unsafe { core::slice::from_raw_parts(p, len) };
        Self::from_bytes(bytes)
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }

    /// Value bytes for BPF lookup (caller may write through the pointer).
    fn as_ptr(&self) -> *mut u8 {
        self.data.as_ptr() as *mut u8
    }
}

/// Byte-keyed BPF hash map (fixed capacity).
pub struct BpfHashMap {
    pub key_size: usize,
    pub value_size: usize,
    pub max_entries: usize,
    table: BpfHashTable,
}

/// Userspace-facing surface (hold `POOL` write lock when mutating).
impl BpfHashMap {
    /// Start a fluent create (allocate + pool insert + optional name).
    pub fn build() -> BpfHashMapBuilder {
        BpfHashMapBuilder::default()
    }

    /// Direct hashbrown table.
    #[inline]
    pub fn table_mut(&mut self) -> &mut BpfHashTable {
        &mut self.table
    }
}

/// Helper / AOT path only (`bpf_for_each_map_elem`).
impl BpfHashMap {
    pub(super) fn for_each_elems(
        &mut self,
        map_arg: usize,
        cb: extern "C" fn(usize, usize, usize, usize) -> i32,
        callback_ctx: usize,
    ) -> usize {
        let keys: heapless::Vec<KeyBlob, 64> = {
            let mut ks = heapless::Vec::new();
            for k in self.table.keys() {
                let _ = ks.push(*k);
            }
            ks
        };
        for k in keys.iter() {
            let key_ptr = k.data.as_ptr() as usize;
            let val_ptr = match self.table.get_mut(k) {
                Some(v) => v.as_mut_ptr() as usize,
                None => continue,
            };
            let rc = cb(map_arg, key_ptr, val_ptr, callback_ctx);
            if rc == 1 {
                break;
            }
        }
        0
    }
}

impl MapOps for BpfHashMap {
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapError> {
        let Some(k) = (unsafe { KeyBlob::from_ptr(key, self.key_size) }) else {
            let e = MapError::InvalidArg;
            e.log_if_bug("hash.lookup");
            return Err(e);
        };
        match self.table.get(&k) {
            Some(v) => Ok(v.as_ptr()),
            None => Err(MapError::NotFound),
        }
    }

    fn update(&mut self, key: *const u8, value: *const u8, flags: u64) -> Result<(), MapError> {
        let Some(k) = (unsafe { KeyBlob::from_ptr(key, self.key_size) }) else {
            let e = MapError::InvalidArg;
            e.log_if_bug("hash.update");
            return Err(e);
        };
        let Some(v) = (unsafe { ValSlot::from_ptr(value, self.value_size) }) else {
            let e = MapError::InvalidArg;
            e.log_if_bug("hash.update");
            return Err(e);
        };
        let exists = self.table.contains_key(&k);
        if flags == BPF_NOEXIST && exists {
            return Err(MapError::Exists);
        }
        if flags == BPF_EXIST && !exists {
            return Err(MapError::NotFound);
        }
        if !exists && self.table.len() >= self.max_entries {
            let e = MapError::NoMem;
            e.log_if_bug("hash.update");
            return Err(e);
        }
        self.table.insert(k, v);
        Ok(())
    }

    fn delete(&mut self, key: *const u8) -> Result<(), MapError> {
        let Some(k) = (unsafe { KeyBlob::from_ptr(key, self.key_size) }) else {
            let e = MapError::InvalidArg;
            e.log_if_bug("hash.delete");
            return Err(e);
        };
        if self.table.remove(&k).is_some() {
            Ok(())
        } else {
            Err(MapError::NotFound)
        }
    }
}

/// Fluent factory for [`BpfHashMap`] (userspace / control plane).
#[derive(Clone, Copy, Default)]
pub struct BpfHashMapBuilder {
    key_size: Option<usize>,
    value_size: Option<usize>,
    id: Option<ResourceId>,
    name: Option<&'static str>,
}

/// Builder failure (incomplete spec, OOM, or name table full).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashBuildError {
    MissingKey,
    MissingValue,
    MissingId,
    BadSize,
    Oom,
    NameFull,
}

impl From<AllocError> for HashBuildError {
    fn from(_: AllocError) -> Self {
        Self::Oom
    }
}

impl BpfHashMapBuilder {
    /// `sizeof(K)` as key width (`1..=MAX_KEY_BYTES`).
    pub fn with_key<K>(mut self) -> Self {
        self.key_size = Some(size_of::<K>());
        self
    }

    /// Raw key width (codegen / dynamic ctrl). Prefer [`Self::with_key`].
    pub fn with_key_size(mut self, key_size: usize) -> Self {
        self.key_size = Some(key_size);
        self
    }

    /// `sizeof(V)` as value width (`1..=MAX_VAL_BYTES`).
    pub fn with_value<V>(mut self) -> Self {
        self.value_size = Some(size_of::<V>());
        self
    }

    /// Raw value width. Prefer [`Self::with_value`].
    pub fn with_value_size(mut self, value_size: usize) -> Self {
        self.value_size = Some(value_size);
        self
    }

    pub fn with_id(mut self, id: ResourceId) -> Self {
        self.id = Some(id);
        self
    }

    /// Registered after a successful pool insert (`register_map_name`).
    pub fn with_name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    fn finish_spec(
        self,
        capacity: usize,
    ) -> Result<(ResourceId, usize, usize, Option<&'static str>), HashBuildError> {
        let key_size = self.key_size.ok_or(HashBuildError::MissingKey)?;
        let value_size = self.value_size.ok_or(HashBuildError::MissingValue)?;
        let id = self.id.ok_or(HashBuildError::MissingId)?;
        if capacity == 0
            || key_size == 0
            || key_size > MAX_KEY_BYTES
            || value_size == 0
            || value_size > MAX_VAL_BYTES
        {
            return Err(HashBuildError::BadSize);
        }
        Ok((id, key_size, value_size, self.name))
    }

    /// Allocate the map, insert into the pool, optionally bind `name`.
    pub async fn build_with_capacity(self, capacity: usize) -> Result<ResourceId, HashBuildError> {
        let (id, key_size, value_size, name) = self.finish_spec(capacity)?;
        let (raw, layout, key_size, value_size) = alloc_bpf_hash(key_size, value_size, capacity)?;
        RawResource::insert(
            id,
            raw,
            layout,
            drop_hash,
            Some(MapKind::Hash),
            key_size,
            value_size,
            value_size,
        )
        .await?;
        if let Some(n) = name {
            if !register_map_name(n, id) {
                error!("[hash] build: register_map_name({n}) failed");
                let _ = RawResource::destroy(id).await;
                return Err(HashBuildError::NameFull);
            }
        }
        Ok(id)
    }
}

unsafe fn drop_hash(ptr: *mut u8) {
    // SAFETY: paired with alloc_bpf_hash `Box::into_raw`; preflight in reclaim_talc_box.
    unsafe { super::pool::reclaim_talc_box::<BpfHashMap>(ptr, "BpfHashMap") };
}

fn alloc_bpf_hash(
    key_size: usize,
    value_size: usize,
    max_entries: usize,
) -> Result<(*mut u8, Layout, usize, usize), AllocError> {
    if key_size == 0 || key_size > MAX_KEY_BYTES || value_size == 0 || value_size > MAX_VAL_BYTES {
        return Err(AllocError);
    }
    let mut table = HashMap::new_in(TalcAlloc);
    table.try_reserve(max_entries).map_err(|_| AllocError)?;
    let map = BpfHashMap {
        key_size,
        value_size,
        max_entries,
        table,
    };
    let boxed = Box::try_new_in(map, allocator())?;
    let raw = Box::into_raw(boxed) as *mut u8;
    let layout = Layout::new::<BpfHashMap>();
    Ok((raw, layout, key_size, value_size))
}

/// Create a fixed-capacity BPF hash map and register it under `id`.
///
/// Prefer [`BpfHashMap::build`] for typed userspace creates; this remains for
/// `bpf.toml` codegen and [`super::ctrl`].
pub async fn create_bpf_hash(
    id: ResourceId,
    key_size: usize,
    value_size: usize,
    max_entries: usize,
) -> Result<(), AllocError> {
    BpfHashMap::build()
        .with_id(id)
        .with_key_size(key_size)
        .with_value_size(value_size)
        .build_with_capacity(max_entries)
        .await
        .map(|_| ())
        .map_err(|e| match e {
            HashBuildError::Oom => AllocError,
            other => {
                error!("[hash] create_bpf_hash: {:?}", other);
                AllocError
            }
        })
}
