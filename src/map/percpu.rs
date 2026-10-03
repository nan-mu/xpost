//! Multicore / `BPF_MAP_TYPE_PERCPU_ARRAY` extension stubs.
//!
//! Gated behind Cargo feature `multicore`. The first-case ESP32-C3 safety
//! contract uses single-core [`super::array::BpfArrayMap`] only; this module
//! documents the future PERCPU surface without implementing shard storage,
//! cross-core merge, or helper dispatch.

#![cfg(feature = "multicore")]

use super::ops::{MapError, MapOps};
use super::pool::ResourceId;

/// Placeholder description of a per-CPU array shard layout.
///
/// When implemented, each logical key indexes `num_cpus` value slots. On the
/// current single-core ESP32-C3 path this type must not be constructed.
#[derive(Clone, Copy, Debug)]
pub struct PercpuArraySpec {
    pub value_size: usize,
    pub max_entries: usize,
    pub num_cpus: usize,
}

/// Unimplemented PERCPU_ARRAY map handle.
///
/// All [`MapOps`] return [`MapError::Unsupported`]. Enable only for API
/// sketches and compile-time exploration.
pub struct BpfPercpuArrayMap {
    pub spec: PercpuArraySpec,
    pub id: ResourceId,
}

impl BpfPercpuArrayMap {
    /// Construct a placeholder; does not allocate shard storage.
    pub const fn new(id: ResourceId, spec: PercpuArraySpec) -> Self {
        Self { spec, id }
    }
}

impl MapOps for BpfPercpuArrayMap {
    fn lookup(&self, _key: *const u8) -> Result<*mut u8, MapError> {
        Err(MapError::Unsupported)
    }

    fn update(
        &mut self,
        _key: *const u8,
        _value: *const u8,
        _flags: u64,
    ) -> Result<(), MapError> {
        Err(MapError::Unsupported)
    }

    fn delete(&mut self, _key: *const u8) -> Result<(), MapError> {
        Err(MapError::Unsupported)
    }
}

/// Future helper entry for `bpf_map_lookup_elem` on a PERCPU_ARRAY.
///
/// Returns null until a real backend exists.
pub fn helper_lookup_percpu(_map: usize, _key: usize) -> usize {
    0
}
