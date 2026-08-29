//! Userspace access: write-lock then touch native containers.
//!
//! # Datapath contract
//!
//! - **Hash / Array** ([`with_hash_map`] / [`with_array_map`]): only while the XDP
//!   datapath is **stopped** ([`crate::datapath::require_stopped`]). Use
//!   boot seed before [`crate::datapath::enable`], or
//!   [`crate::datapath::disable`] before mutating rules.
//! - **Ringbuf** ([`with_ringbuf_map`]): allowed while datapath is running.
//!   Use [`BpfRingBuf::try_consume`] to drain.

use log::error;

use crate::datapath;

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
        Some(MapKind::ProgArray) => "PROG_ARRAY",
        Some(MapKind::WdtFeed) => "WDT_FEED",
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

/// Read-only ARRAY element as `u64` (key = index). Allowed while datapath runs.
pub fn peek_array_u64(name: &str, index: u32) -> Result<u64, MapError> {
    let id = resolve_named_id(name)?;
    let guard = POOL
        .try_read()
        .ok_or(MapError::Busy)?;
    let table = match guard.as_ref() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match table.get(&id) {
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
    let map = unsafe { resource.as_ref::<BpfArrayMap>() };
    let ptr = map.elem_ptr(index as usize);
    if ptr.is_null() {
        return Err(MapError::NotFound);
    }
    // SAFETY: single-core MCU; BPF may increment concurrently — stats read is best-effort.
    let v = unsafe { core::ptr::read_volatile(ptr.cast::<u64>()) };
    Ok(v)
}

/// `POOL.write` then `&mut BpfArrayMap`.
///
/// Requires XDP datapath stopped.
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

/// `POOL.write` then `&mut BpfRingBuf`.
///
/// Allowed while datapath is running. Use [`BpfRingBuf::try_consume`] to drain.
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
