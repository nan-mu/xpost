//! Userspace access: write-lock then touch native containers.
//!
//! # Datapath contract
//!
//! - **Hash / Array** ([`with_hash_map`] / [`with_array_map`]): only while the XDP
//!   datapath is **stopped** ([`crate::patch::datapath::require_stopped`]). Use
//!   boot seed before [`crate::patch::datapath::enable`], or
//!   [`crate::patch::datapath::disable`] before mutating rules.
//! - **Ringbuf** ([`with_ringbuf_map`]): allowed while datapath is running (XDP →
//!   userspace path). Full SPSC consumer is not implemented here yet.
//!
//! Sync backends:
//! - `patch-sync-embassy` (default): async `POOL.write().await`
//! - `patch-sync-st`: sync `POOL.write()`

use log::error;

use crate::patch::datapath;

use super::array::BpfArrayMap;
use super::hash::BpfHashMap;
use super::names::resolve_map_name;
use super::ops::MapError;
use super::pool::{MapKind, POOL};
use super::ringbuf::BpfRingBuf;

fn kind_label(kind: Option<MapKind>) -> &'static str {
    match kind {
        Some(MapKind::Hash) => "HASH",
        Some(MapKind::Array) => "ARRAY",
        Some(MapKind::Ringbuf) => "RINGBUF",
        Some(MapKind::Devmap) => "DEVMAP",
        None => "none",
    }
}

fn resolve_named_id(name: &str) -> Result<super::pool::ResourceId, MapError> {
    let Some(id) = resolve_map_name(name) else {
        error!("[userspace] map name {:?} not registered", name);
        return Err(MapError::NotFound);
    };
    Ok(id)
}

/// `POOL.write` then `&mut BpfHashMap` (use [`BpfHashMap::table_mut`]).
///
/// Requires XDP datapath stopped.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_hash_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfHashMap) -> R,
) -> Result<R, MapError> {
    datapath::require_stopped()?;
    let id = resolve_named_id(name)?;
    let mut guard = POOL.write().await;
    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get_mut(&id) {
        Some(r) => r,
        None => {
            error!("[userspace] map id {} missing from pool", id);
            return Err(MapError::NotFound);
        }
    };
    if resource.kind != Some(MapKind::Hash) {
        error!(
            "[userspace] map {:?} kind mismatch (want HASH, got {})",
            name,
            kind_label(resource.kind)
        );
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfHashMap>() }))
}

/// `POOL.write` then `&mut BpfHashMap` (use [`BpfHashMap::table_mut`]).
///
/// Requires XDP datapath stopped.
#[cfg(feature = "patch-sync-st")]
pub fn with_hash_map<R>(name: &str, f: impl FnOnce(&mut BpfHashMap) -> R) -> Result<R, MapError> {
    datapath::require_stopped()?;
    let id = resolve_named_id(name)?;
    let mut guard = POOL.write();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get_mut(&id) {
        Some(r) => r,
        None => {
            error!("[userspace] map id {} missing from pool", id);
            return Err(MapError::NotFound);
        }
    };
    if resource.kind != Some(MapKind::Hash) {
        error!(
            "[userspace] map {:?} kind mismatch (want HASH, got {})",
            name,
            kind_label(resource.kind)
        );
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfHashMap>() }))
}

/// `POOL.write` then `&mut BpfArrayMap`.
///
/// Requires XDP datapath stopped.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_array_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfArrayMap) -> R,
) -> Result<R, MapError> {
    datapath::require_stopped()?;
    let id = resolve_named_id(name)?;
    let mut guard = POOL.write().await;
    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get_mut(&id) {
        Some(r) => r,
        None => {
            error!("[userspace] map id {} missing from pool", id);
            return Err(MapError::NotFound);
        }
    };
    match resource.kind {
        Some(MapKind::Array) | Some(MapKind::Devmap) => {}
        got => {
            error!(
                "[userspace] map {:?} kind mismatch (want ARRAY|DEVMAP, got {})",
                name,
                kind_label(got)
            );
            return Err(MapError::Unsupported);
        }
    }
    Ok(f(unsafe { resource.as_mut::<BpfArrayMap>() }))
}

/// `POOL.write` then `&mut BpfArrayMap`.
///
/// Requires XDP datapath stopped.
#[cfg(feature = "patch-sync-st")]
pub fn with_array_map<R>(name: &str, f: impl FnOnce(&mut BpfArrayMap) -> R) -> Result<R, MapError> {
    datapath::require_stopped()?;
    let id = resolve_named_id(name)?;
    let mut guard = POOL.write();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get_mut(&id) {
        Some(r) => r,
        None => {
            error!("[userspace] map id {} missing from pool", id);
            return Err(MapError::NotFound);
        }
    };
    match resource.kind {
        Some(MapKind::Array) | Some(MapKind::Devmap) => {}
        got => {
            error!(
                "[userspace] map {:?} kind mismatch (want ARRAY|DEVMAP, got {})",
                name,
                kind_label(got)
            );
            return Err(MapError::Unsupported);
        }
    }
    Ok(f(unsafe { resource.as_mut::<BpfArrayMap>() }))
}

/// `POOL.write` then `&mut BpfRingBuf`.
///
/// Allowed while datapath is running. Full SPSC consumer is not implemented yet.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn with_ringbuf_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfRingBuf) -> R,
) -> Result<R, MapError> {
    let id = resolve_named_id(name)?;
    let mut guard = POOL.write().await;
    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get_mut(&id) {
        Some(r) => r,
        None => {
            error!("[userspace] map id {} missing from pool", id);
            return Err(MapError::NotFound);
        }
    };
    if resource.kind != Some(MapKind::Ringbuf) {
        error!(
            "[userspace] map {:?} kind mismatch (want RINGBUF, got {})",
            name,
            kind_label(resource.kind)
        );
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfRingBuf>() }))
}

/// `POOL.write` then `&mut BpfRingBuf`.
///
/// Allowed while datapath is running. Full SPSC consumer is not implemented yet.
#[cfg(feature = "patch-sync-st")]
pub fn with_ringbuf_map<R>(
    name: &str,
    f: impl FnOnce(&mut BpfRingBuf) -> R,
) -> Result<R, MapError> {
    let id = resolve_named_id(name)?;
    let mut guard = POOL.write();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get_mut(&id) {
        Some(r) => r,
        None => {
            error!("[userspace] map id {} missing from pool", id);
            return Err(MapError::NotFound);
        }
    };
    if resource.kind != Some(MapKind::Ringbuf) {
        error!(
            "[userspace] map {:?} kind mismatch (want RINGBUF, got {})",
            name,
            kind_label(resource.kind)
        );
        return Err(MapError::Unsupported);
    }
    Ok(f(unsafe { resource.as_mut::<BpfRingBuf>() }))
}
