//! BPF_MAP_TYPE_HASH — byte-keyed hash table in the talc pool.
//!
//! Also hosts the generic [`SharedHashMap`] resource used by pool self-checks.

#![allow(unsafe_op_in_unsafe_fn)]

use core::alloc::Layout;
use core::hash::{Hash, Hasher};
use core::ptr;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use hashbrown::HashMap;

use super::abi::{BPF_EXIST, BPF_NOEXIST};
use super::pool::{allocator, MapKind, RawResource, ResourceId, TalcAlloc};

const MAX_KEY_BYTES: usize = 64;
const MAX_VAL_BYTES: usize = 256;

/// A pre-allocated HashMap that lives inside the talc pool.
pub type SharedHashMap<K, V> = HashMap<K, V, hashbrown::DefaultHashBuilder, TalcAlloc>;

unsafe fn drop_hashmap<K, V>(ptr: *mut u8) {
    let _ = unsafe { Box::from_raw_in(ptr as *mut SharedHashMap<K, V>, allocator()) };
}

fn alloc_hashmap<K, V>(
    capacity: usize,
) -> Result<(*mut u8, Layout), AllocError>
where
    K: core::hash::Hash + Eq + Send + 'static,
    V: Send + 'static,
{
    let mut map: SharedHashMap<K, V> = HashMap::new_in(TalcAlloc);
    map.try_reserve(capacity).map_err(|_| AllocError)?;
    let boxed = Box::try_new_in(map, allocator())?;
    let raw_ptr = Box::into_raw(boxed) as *mut u8;
    let layout = Layout::new::<SharedHashMap<K, V>>();
    Ok((raw_ptr, layout))
}

/// Create a HashMap pre-allocated for `capacity` entries and register it.
///
/// Capacity is reserved up-front via `try_reserve`; no further growth is
/// permitted by the resource contract.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn create_hashmap<K, V>(id: ResourceId, capacity: usize) -> Result<(), AllocError>
where
    K: core::hash::Hash + Eq + Send + 'static,
    V: Send + 'static,
{
    let (raw_ptr, layout) = alloc_hashmap::<K, V>(capacity)?;
    RawResource::insert(id, raw_ptr, layout, drop_hashmap::<K, V>, None, 0, 0, 0).await
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn create_hashmap<K, V>(id: ResourceId, capacity: usize) -> Result<(), AllocError>
where
    K: core::hash::Hash + Eq + Send + 'static,
    V: Send + 'static,
{
    let (raw_ptr, layout) = alloc_hashmap::<K, V>(capacity)?;
    RawResource::insert(id, raw_ptr, layout, drop_hashmap::<K, V>, None, 0, 0, 0)
}

#[derive(Clone, Copy)]
pub(super) struct KeyBlob {
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
    fn from_ptr(p: *const u8, len: usize) -> Option<Self> {
        if p.is_null() || len == 0 || len > MAX_KEY_BYTES {
            return None;
        }
        let mut data = [0u8; MAX_KEY_BYTES];
        unsafe { ptr::copy_nonoverlapping(p, data.as_mut_ptr(), len) };
        Some(Self {
            len: len as u8,
            data,
        })
    }
}

struct ValSlot {
    #[allow(dead_code)]
    len: usize,
    data: [u8; MAX_VAL_BYTES],
}

impl ValSlot {
    fn from_ptr(p: *const u8, len: usize) -> Option<Self> {
        if p.is_null() || len == 0 || len > MAX_VAL_BYTES {
            return None;
        }
        let mut data = [0u8; MAX_VAL_BYTES];
        unsafe { ptr::copy_nonoverlapping(p, data.as_mut_ptr(), len) };
        Some(Self { len, data })
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.data.as_mut_ptr()
    }
}

/// Byte-keyed BPF hash map (fixed capacity).
pub struct BpfHashMap {
    key_size: usize,
    value_size: usize,
    max_entries: usize,
    table: HashMap<KeyBlob, ValSlot, hashbrown::DefaultHashBuilder, TalcAlloc>,
}

impl BpfHashMap {
    /// `bpf_map_lookup_elem`.
    pub fn lookup(&mut self, key: *const u8) -> *mut u8 {
        let Some(k) = KeyBlob::from_ptr(key, self.key_size) else {
            return ptr::null_mut();
        };
        match self.table.get_mut(&k) {
            Some(v) => v.as_mut_ptr(),
            None => ptr::null_mut(),
        }
    }

    /// `bpf_map_update_elem`.
    pub fn update(&mut self, key: *const u8, value: *const u8, flags: u64) -> i32 {
        let Some(k) = KeyBlob::from_ptr(key, self.key_size) else {
            return -22; // EINVAL
        };
        let Some(v) = ValSlot::from_ptr(value, self.value_size) else {
            return -22;
        };
        let exists = self.table.contains_key(&k);
        if flags == BPF_NOEXIST && exists {
            return -17; // EEXIST
        }
        if flags == BPF_EXIST && !exists {
            return -2; // ENOENT
        }
        if !exists && self.table.len() >= self.max_entries {
            return -12; // ENOMEM
        }
        self.table.insert(k, v);
        0
    }

    /// `bpf_map_delete_elem`.
    pub fn delete(&mut self, key: *const u8) -> i32 {
        let Some(k) = KeyBlob::from_ptr(key, self.key_size) else {
            return -22;
        };
        if self.table.remove(&k).is_some() {
            0
        } else {
            -2
        }
    }

    /// `bpf_for_each_map_elem` walk.
    pub fn for_each_elems(
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

unsafe fn drop_hash(ptr: *mut u8) {
    let _ = unsafe { Box::from_raw_in(ptr as *mut BpfHashMap, allocator()) };
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
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn create_bpf_hash(
    id: ResourceId,
    key_size: usize,
    value_size: usize,
    max_entries: usize,
) -> Result<(), AllocError> {
    let (raw, layout, key_size, value_size) = alloc_bpf_hash(key_size, value_size, max_entries)?;
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
    .await
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn create_bpf_hash(
    id: ResourceId,
    key_size: usize,
    value_size: usize,
    max_entries: usize,
) -> Result<(), AllocError> {
    let (raw, layout, key_size, value_size) = alloc_bpf_hash(key_size, value_size, max_entries)?;
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
}
