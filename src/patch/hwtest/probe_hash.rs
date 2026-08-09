//! Talc-pool probe: typed hashbrown map (not BPF `BpfHashMap`).

use core::alloc::Layout;

use allocator_api2::alloc::AllocError;
use allocator_api2::boxed::Box;
use hashbrown::HashMap;

use crate::patch::map::pool::{RawResource, ResourceId, TalcAlloc, allocator, reclaim_talc_box};

/// Pre-reserved `HashMap` living in the talc pool — used only by pool self-checks.
pub type ProbeHashMap<K, V> = HashMap<K, V, hashbrown::DefaultHashBuilder, TalcAlloc>;

unsafe fn drop_probe_hashmap<K, V>(ptr: *mut u8) {
    unsafe { reclaim_talc_box::<ProbeHashMap<K, V>>(ptr, "ProbeHashMap") };
}

fn alloc_probe_hashmap<K, V>(capacity: usize) -> Result<(*mut u8, Layout), AllocError>
where
    K: core::hash::Hash + Eq + Send + 'static,
    V: Send + 'static,
{
    let mut map: ProbeHashMap<K, V> = HashMap::new_in(TalcAlloc);
    map.try_reserve(capacity).map_err(|_| AllocError)?;
    let boxed = Box::try_new_in(map, allocator())?;
    let raw_ptr = Box::into_raw(boxed) as *mut u8;
    let layout = Layout::new::<ProbeHashMap<K, V>>();
    Ok((raw_ptr, layout))
}

/// Insert a capacity-reserved probe hashmap (`kind = None`).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn create_probe_hashmap<K, V>(id: ResourceId, capacity: usize) -> Result<(), AllocError>
where
    K: core::hash::Hash + Eq + Send + 'static,
    V: Send + 'static,
{
    let (raw_ptr, layout) = alloc_probe_hashmap::<K, V>(capacity)?;
    RawResource::insert(
        id,
        raw_ptr,
        layout,
        drop_probe_hashmap::<K, V>,
        None,
        0,
        0,
        0,
    )
    .await
}

#[cfg(feature = "patch-sync-st")]
pub fn create_probe_hashmap<K, V>(id: ResourceId, capacity: usize) -> Result<(), AllocError>
where
    K: core::hash::Hash + Eq + Send + 'static,
    V: Send + 'static,
{
    let (raw_ptr, layout) = alloc_probe_hashmap::<K, V>(capacity)?;
    RawResource::insert(
        id,
        raw_ptr,
        layout,
        drop_probe_hashmap::<K, V>,
        None,
        0,
        0,
        0,
    )
}
