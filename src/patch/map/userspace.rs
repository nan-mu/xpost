//! Userspace access: write-lock then touch native containers.
//!
//! - `patch-sync-embassy` (default): async `POOL.write().await`
//! - `patch-sync-st`: sync `POOL.write()`

use log::error;

use super::array::BpfArrayMap;
use super::hash::BpfHashMap;
use super::hash::{KeyBlob, ValSlot};
use super::names::resolve_map_name;
use super::ops::MapError;
use super::pool::{MapKind, POOL};
use super::ringbuf::BpfRingBuf;
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

/// `POOL.write` then `&mut BpfHashMap` (use [`BpfHashMap::table_mut`]).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_hash_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfHashMap) -> R,
) -> Result<R, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    let mut guard = POOL.write().await;
    let table = guard.as_mut().ok_or(MapError::InvalidArg)?;
    let resource = table.get_mut(&id).ok_or_else(|| {
        error!("[userspace] map id {} missing", id);
        MapError::NotFound
    })?;
    if resource.kind != Some(MapKind::Hash) {
        error!("[userspace] map {:?} is not HASH", name);
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfHashMap>() }))
}

/// `POOL.write` then `&mut BpfHashMap` (use [`BpfHashMap::table_mut`]).
#[cfg(feature = "patch-sync-st")]
pub fn with_hash_map<R>(name: &str, f: impl FnOnce(&mut BpfHashMap) -> R) -> Result<R, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    let mut guard = POOL.write();
    let table = guard.as_mut().ok_or(MapError::InvalidArg)?;
    let resource = table.get_mut(&id).ok_or_else(|| {
        error!("[userspace] map id {} missing", id);
        MapError::NotFound
    })?;
    if resource.kind != Some(MapKind::Hash) {
        error!("[userspace] map {:?} is not HASH", name);
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfHashMap>() }))
}

/// `POOL.write` then `&mut BpfArrayMap`.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_array_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfArrayMap) -> R,
) -> Result<R, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    let mut guard = POOL.write().await;
    let table = guard.as_mut().ok_or(MapError::InvalidArg)?;
    let resource = table.get_mut(&id).ok_or_else(|| {
        error!("[userspace] map id {} missing", id);
        MapError::NotFound
    })?;
    match resource.kind {
        Some(MapKind::Array) | Some(MapKind::Devmap) => {}
        _ => {
            error!("[userspace] map {:?} is not ARRAY/DEVMAP", name);
            return Err(MapError::Unsupported);
        }
    }
    Ok(f(unsafe { resource.as_mut::<BpfArrayMap>() }))
}

/// `POOL.write` then `&mut BpfArrayMap`.
#[cfg(feature = "patch-sync-st")]
pub fn with_array_map<R>(name: &str, f: impl FnOnce(&mut BpfArrayMap) -> R) -> Result<R, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    let mut guard = POOL.write();
    let table = guard.as_mut().ok_or(MapError::InvalidArg)?;
    let resource = table.get_mut(&id).ok_or_else(|| {
        error!("[userspace] map id {} missing", id);
        MapError::NotFound
    })?;
    match resource.kind {
        Some(MapKind::Array) | Some(MapKind::Devmap) => {}
        _ => {
            error!("[userspace] map {:?} is not ARRAY/DEVMAP", name);
            return Err(MapError::Unsupported);
        }
    }
    Ok(f(unsafe { resource.as_mut::<BpfArrayMap>() }))
}

/// `POOL.write` then `&mut BpfRingBuf`.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_ringbuf_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfRingBuf) -> R,
) -> Result<R, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    let mut guard = POOL.write().await;
    let table = guard.as_mut().ok_or(MapError::InvalidArg)?;
    let resource = table.get_mut(&id).ok_or_else(|| {
        error!("[userspace] map id {} missing", id);
        MapError::NotFound
    })?;
    if resource.kind != Some(MapKind::Ringbuf) {
        error!("[userspace] map {:?} is not RINGBUF", name);
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfRingBuf>() }))
}

/// `POOL.write` then `&mut BpfRingBuf`.
#[cfg(feature = "patch-sync-st")]
pub fn with_ringbuf_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfRingBuf) -> R,
) -> Result<R, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    let mut guard = POOL.write();
    let table = guard.as_mut().ok_or(MapError::InvalidArg)?;
    let resource = table.get_mut(&id).ok_or_else(|| {
        error!("[userspace] map id {} missing", id);
        MapError::NotFound
    })?;
    if resource.kind != Some(MapKind::Ringbuf) {
        error!("[userspace] map {:?} is not RINGBUF", name);
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfRingBuf>() }))
}
