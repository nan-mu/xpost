//! Userspace access: write-lock then touch native containers.
//!
//! # Runtime contract
//!
//! - **Hash / Array** ([`with_hash_map`] / [`with_array_map`]): only while the XDP
//!   XDP is not leased. Seed them before `XdpRt::lease`, or after
//!   `XdpRt::release`.
//! - **Ringbuf** ([`with_ringbuf_map`]): allowed while datapath is running.
//!   Use [`BpfRingBuf::try_consume`] to drain.

use log::error;

use super::array::BpfArrayMap;
use super::hash::BpfHashMap;
use super::names::resolve_map_name;
use super::ops::MapError;
use super::pool::{MapKind, pool_try_read};
use super::ringbuf::{BpfRingBuf, RingbufMap};
use super::runtime::{self, MAP_RT};

/// Hash and array mutation shares storage with AOT helpers.  The runtime
/// lease is the only authority that grants those helpers access.
fn require_unleased() -> Result<(), MapError> {
    if crate::xdp::is_leased() {
        return Err(MapError::Busy);
    }
    Ok(())
}

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
    require_unleased()?;
    let id = resolve_named_id(name)?;
    let mut guard = MAP_RT.write().await;
    let rt = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match rt.meta.get_mut(id) {
        Some(m) => &mut m.storage,
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
    let kind = runtime::try_read_storage(id, |r| r.kind).flatten();
    match kind {
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
    let ptr = runtime::try_read_storage(id, |r| r.ptr as *const BpfArrayMap)
        .ok_or(MapError::Busy)?;
    let map = unsafe { &*ptr };
    let ptr = map.elem_ptr(index as usize);
    if ptr.is_null() {
        return Err(MapError::NotFound);
    }
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
    require_unleased()?;
    let id = resolve_named_id(name)?;
    let mut guard = MAP_RT.write().await;
    let rt = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match rt.meta.get_mut(id) {
        Some(m) => &mut m.storage,
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
    let mut guard = MAP_RT.write().await;
    let rt = match guard.as_mut() {
        Some(t) => t,
        None => {
            error!("[userspace] pool not initialized");
            return Err(MapError::InvalidArg);
        }
    };
    let resource = match rt.meta.get_mut(id) {
        Some(m) => &mut m.storage,
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
    Ok(f(unsafe { resource.as_mut::<RingbufMap>() }.bpf_mut()))
}
