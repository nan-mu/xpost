//! Userspace (embassy control-plane) access to BPF maps by name.

use super::hash::{BpfHashMap, KeyBlob, ValSlot};
use super::names::resolve_map_name;
use super::pool::{MapKind, POOL};
use super::{FirewallRulesKey, FirewallRulesValue};

impl From<FirewallRulesKey> for KeyBlob {
    fn from(k: FirewallRulesKey) -> Self {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&raw const k) as *const u8,
                core::mem::size_of::<FirewallRulesKey>(),
            )
        };
        KeyBlob::from_bytes(bytes).expect("FirewallRulesKey fits KeyBlob")
    }
}

impl From<FirewallRulesValue> for ValSlot {
    fn from(v: FirewallRulesValue) -> Self {
        ValSlot::from_bytes(core::slice::from_ref(&v.v)).expect("u8 fits ValSlot")
    }
}

impl From<u8> for ValSlot {
    fn from(v: u8) -> Self {
        ValSlot::from_bytes(core::slice::from_ref(&v)).expect("u8 fits ValSlot")
    }
}

/// Run `f` on a HASH map looked up by bpf.toml name under a `POOL` read lock.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_hash_map<R>(name: &str, f: impl FnOnce(&BpfHashMap) -> R) -> Option<R> {
    let id = resolve_map_name(name)?;
    let guard = POOL.read().await;
    let table = guard.as_ref()?;
    let resource = table.get(&id)?;
    if resource.kind != Some(MapKind::Hash) {
        return None;
    }
    Some(f(unsafe { resource.as_ref::<BpfHashMap>() }))
}

#[cfg(feature = "patch-sync-st")]
pub fn with_hash_map<R>(name: &str, f: impl FnOnce(&BpfHashMap) -> R) -> Option<R> {
    let id = resolve_map_name(name)?;
    let guard = POOL.read();
    let table = guard.as_ref()?;
    let resource = table.get(&id)?;
    if resource.kind != Some(MapKind::Hash) {
        return None;
    }
    Some(f(unsafe { resource.as_ref::<BpfHashMap>() }))
}

/// Convenience: read-lock `name`, then [`BpfHashMap::insert_entry`].
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn hash_insert(
    name: &str,
    key: impl Into<KeyBlob>,
    val: impl Into<ValSlot>,
) -> Option<i32> {
    let key = key.into();
    let val = val.into();
    with_hash_map(name, |m| m.insert_entry(key, val)).await
}

#[cfg(feature = "patch-sync-st")]
pub fn hash_insert(name: &str, key: impl Into<KeyBlob>, val: impl Into<ValSlot>) -> Option<i32> {
    let key = key.into();
    let val = val.into();
    with_hash_map(name, |m| m.insert_entry(key, val))
}
