//! Dynamic map create / destroy (network control-plane facing).
//!
//! Specs mirror `bpf.toml` map fields without relying on `build.rs` codegen.
//! Destroy refuses while any XDP program still holds the map (`users > 0`).

use allocator_api2::alloc::AllocError;

use super::array::{create_bpf_array, create_bpf_devmap, create_bpf_prog_array};
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
    /// `register_map_name` table full.
    NameTableFull,
    /// Map name already registered (use [`destroy_map`] first).
    NameTaken,
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
        MapKind::ProgArray => {
            if cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            create_bpf_prog_array(id, cmd.max_entries).await?;
        }
        MapKind::WdtFeed => {
            if cmd.max_entries == 0 {
                return Err(MapCtrlError::BadSpec);
            }
            super::wdt_feed::create_bpf_wdt_feed(id, cmd.max_entries).await?;
        }
    }

    Ok(id)
}

/// Register a named map: allocate in the pool and bind `name` → [`ResourceId`].
///
/// Embedders pass only the logical name and layout spec — no AOT VMA. Symbol
/// aliases are published when an XDP image loads ([`super::abi::bind_map_symbols`]).
pub async fn register_map(
    name: &'static str,
    cmd: MapCreateCmd,
) -> Result<ResourceId, MapCtrlError> {
    if name.is_empty() {
        return Err(MapCtrlError::BadSpec);
    }
    if super::names::resolve_map_name(name).is_some() {
        return Err(MapCtrlError::NameTaken);
    }
    let id = create_map(cmd).await?;
    if !super::names::register_map_name(name, id) {
        let _ = destroy_map(id).await;
        return Err(MapCtrlError::NameTableFull);
    }
    Ok(id)
}

/// [`register_map`] shorthand for `BPF_MAP_TYPE_RINGBUF` (`max_entries` = byte capacity).
pub async fn register_ringbuf(
    name: &'static str,
    byte_capacity: usize,
) -> Result<ResourceId, MapCtrlError> {
    register_map(
        name,
        MapCreateCmd {
            kind: MapKind::Ringbuf,
            key_size: 0,
            value_size: 0,
            max_entries: byte_capacity,
            id: None,
        },
    )
    .await
}

/// Destroy a map. Fails with [`MapCtrlError::InUse`] if any XDP program holds it.
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
