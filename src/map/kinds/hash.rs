//! BPF_MAP_TYPE_HASH — byte-keyed hash table in the talc pool.

#![allow(unsafe_op_in_unsafe_fn)]

use core::{
    alloc::Layout,
    hash::{Hash, Hasher},
};

use allocator_api2::{alloc::AllocError, boxed::Box};
use hashbrown::HashMap;

use crate::map::{
    MapType,
    heap::{RawResource, TalcAlloc, allocator},
    ops::{BPF_EXIST, BPF_NOEXIST, MapOpError, MapOps},
};

const MAX_KEY_BYTES: usize = 64;
const MAX_VAL_BYTES: usize = 256;

/// Inner hashbrown table of [`BpfHashMap`] (`KeyBlob` → `ValSlot`).
pub(crate) type BpfHashTable = HashMap<KeyBlob, ValSlot, hashbrown::DefaultHashBuilder, TalcAlloc>;

/// Opaque key bytes for [`BpfHashMap`].
#[derive(Clone, Copy)]
pub(crate) struct KeyBlob {
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
pub(crate) struct ValSlot {
    data: [u8; MAX_VAL_BYTES],
}

impl ValSlot {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_VAL_BYTES {
            return None;
        }
        let mut data = [0u8; MAX_VAL_BYTES];
        data[..bytes.len()].copy_from_slice(bytes);
        Some(Self { data })
    }

    unsafe fn from_ptr(p: *const u8, len: usize) -> Option<Self> {
        if p.is_null() || len == 0 || len > MAX_VAL_BYTES {
            return None;
        }
        let bytes = unsafe { core::slice::from_raw_parts(p, len) };
        Self::from_bytes(bytes)
    }

    fn as_ptr(&self) -> *mut u8 {
        self.data.as_ptr() as *mut u8
    }
}

/// Byte-keyed BPF hash map (fixed capacity).
pub(crate) struct BpfHashMap {
    pub(crate) key_size: usize,
    pub(crate) value_size: usize,
    pub(crate) max_entries: usize,
    table: BpfHashTable,
}

impl MapOps for BpfHashMap {
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapOpError> {
        let Some(k) = (unsafe { KeyBlob::from_ptr(key, self.key_size) }) else {
            return Err(MapOpError::InvalidArg);
        };
        match self.table.get(&k) {
            Some(v) => Ok(v.as_ptr()),
            None => Err(MapOpError::NotFound),
        }
    }

    fn update(&mut self, key: *const u8, value: *const u8, flags: u64) -> Result<(), MapOpError> {
        let Some(k) = (unsafe { KeyBlob::from_ptr(key, self.key_size) }) else {
            return Err(MapOpError::InvalidArg);
        };
        let Some(v) = (unsafe { ValSlot::from_ptr(value, self.value_size) }) else {
            return Err(MapOpError::InvalidArg);
        };
        let exists = self.table.contains_key(&k);
        if flags == BPF_NOEXIST && exists {
            return Err(MapOpError::Exists);
        }
        if flags == BPF_EXIST && !exists {
            return Err(MapOpError::NotFound);
        }
        if !exists && self.table.len() >= self.max_entries {
            return Err(MapOpError::NoMem);
        }
        self.table.insert(k, v);
        Ok(())
    }

    fn delete(&mut self, key: *const u8) -> Result<(), MapOpError> {
        let Some(k) = (unsafe { KeyBlob::from_ptr(key, self.key_size) }) else {
            return Err(MapOpError::InvalidArg);
        };
        if self.table.remove(&k).is_some() {
            Ok(())
        } else {
            Err(MapOpError::NotFound)
        }
    }
}

impl BpfHashMap {
    /// Control-plane: drop every entry (capacity unchanged).
    pub(crate) fn clear(&mut self) {
        self.table.clear();
    }

    pub(crate) fn len_used(&self) -> usize {
        self.table.len()
    }

    /// Control-plane: visit each occupied entry (key/value byte slices).
    pub(crate) fn for_each<F: FnMut(&[u8], &[u8])>(&self, mut f: F) {
        for (k, v) in self.table.iter() {
            f(
                &k.data[..k.len as usize],
                &v.data[..self.value_size],
            );
        }
    }
}

unsafe fn drop_hash(ptr: *mut u8) {
    unsafe { crate::map::heap::reclaim_talc_box::<BpfHashMap>(ptr, "BpfHashMap") };
}

fn alloc_bpf_hash(
    key_size: usize,
    value_size: usize,
    max_entries: usize,
) -> Result<(*mut u8, Layout), AllocError> {
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
    Ok((raw, layout))
}

pub(crate) fn alloc_hash_storage(
    key_size: usize,
    value_size: usize,
    max_entries: usize,
) -> Result<RawResource, AllocError> {
    let (raw, layout) = alloc_bpf_hash(key_size, value_size, max_entries)?;
    Ok(RawResource::build(
        raw,
        layout,
        drop_hash,
        Some(MapType::Hash),
    ))
}
