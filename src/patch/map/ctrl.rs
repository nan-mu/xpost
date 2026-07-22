//! Dynamic map create / destroy (network control-plane facing).
//!
//! Specs mirror `bpf.toml` map fields without relying on `build.rs` codegen.
//! Destroy refuses while any XDP program still holds the map (`users > 0`).

use allocator_api2::alloc::AllocError;

use super::array::{create_bpf_array, create_bpf_devmap};
use super::hash::create_bpf_hash;
use super::pool::{MapKind, RawResource, ResourceId};
use super::ringbuf::create_bpf_ringbuf;

/// Upper bound for auto-allocated Ids (dense scan).
pub const MAX_MAP_IDS: ResourceId = 64;

/// In-memory create command (subset of bpf.toml `[maps.*]`).
#[derive(Clone, Copy, Debug)]
pub struct MapCreateCmd {
    pub kind: MapKind,
    /// HASH/ARRAY: sizeof(key). RINGBUF: ignored (0).
    pub key_size: usize,
    /// HASH/ARRAY: sizeof(value). RINGBUF: ignored (0).
    pub value_size: usize,
    /// HASH/ARRAY: entry count. RINGBUF: byte capacity (`max_entries` on Linux).
    pub max_entries: usize,
    /// Prefer this Id; `None` → allocate lowest free in `0..MAX_MAP_IDS`.
    pub id: Option<ResourceId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapCtrlError {
    BadSpec,
    Oom,
    IdTaken,
    NotFound,
    /// XDP program(s) still reference this map.
    InUse,
    NoId,
}

impl From<AllocError> for MapCtrlError {
    fn from(_: AllocError) -> Self {
        Self::Oom
    }
}

/// Create a BPF map from a control-plane command.
///
/// Returns the assigned [`ResourceId`] on success.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn create_map(cmd: MapCreateCmd) -> Result<ResourceId, MapCtrlError> {
    let id = match cmd.id {
        Some(id) => {
            if RawResource::contains(id).await {
                return Err(MapCtrlError::IdTaken);
            }
            id
        }
        None => RawResource::alloc_id(MAX_MAP_IDS)
            .await
            .ok_or(MapCtrlError::NoId)?,
    };

    match cmd.kind {
        MapKind::Array => {
            if cmd.value_size == 0 || cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_array(id, cmd.value_size, cmd.max_entries).await?;
        }
        MapKind::Hash => {
            if cmd.key_size == 0 || cmd.value_size == 0 || cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_hash(id, cmd.key_size, cmd.value_size, cmd.max_entries).await?;
        }
        MapKind::Ringbuf => {
            if cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_ringbuf(id, cmd.max_entries).await?;
        }
        MapKind::Devmap => {
            if cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_devmap(id, cmd.max_entries).await?;
        }
    }

    Ok(id)
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn create_map(cmd: MapCreateCmd) -> Result<ResourceId, MapCtrlError> {
    let id = match cmd.id {
        Some(id) => {
            if RawResource::contains(id) {
                return Err(MapCtrlError::IdTaken);
            }
            id
        }
        None => RawResource::alloc_id(MAX_MAP_IDS).ok_or(MapCtrlError::NoId)?,
    };

    match cmd.kind {
        MapKind::Array => {
            if cmd.value_size == 0 || cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_array(id, cmd.value_size, cmd.max_entries)?;
        }
        MapKind::Hash => {
            if cmd.key_size == 0 || cmd.value_size == 0 || cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_hash(id, cmd.key_size, cmd.value_size, cmd.max_entries)?;
        }
        MapKind::Ringbuf => {
            if cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_ringbuf(id, cmd.max_entries)?;
        }
        MapKind::Devmap => {
            if cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_devmap(id, cmd.max_entries)?;
        }
    }

    Ok(id)
}

/// Destroy a map. Fails with [`MapCtrlError::InUse`] if any XDP program holds it.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn destroy_map(id: ResourceId) -> Result<(), MapCtrlError> {
    match RawResource::users(id).await {
        None => return Err(MapCtrlError::NotFound),
        Some(u) if u > 0 => return Err(MapCtrlError::InUse),
        Some(_) => {}
    }
    if RawResource::destroy(id).await {
        Ok(())
    } else {
        Err(MapCtrlError::NotFound)
    }
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn destroy_map(id: ResourceId) -> Result<(), MapCtrlError> {
    match RawResource::users(id) {
        None => return Err(MapCtrlError::NotFound),
        Some(u) if u > 0 => return Err(MapCtrlError::InUse),
        Some(_) => {}
    }
    if RawResource::destroy(id) {
        Ok(())
    } else {
        Err(MapCtrlError::NotFound)
    }
}
