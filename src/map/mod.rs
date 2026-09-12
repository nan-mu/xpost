//! XDP MAP resource pool + BPF map types + helper ABI.

pub mod builder;
pub mod helper;
pub mod kinds;
pub mod runtime;

#[cfg(feature = "ringbuf")]
pub use builder::RingbufView;
pub use builder::{ArrayView, CompleteBuilder, HashView, MapBuilder as TypedMapBuilder, MapType};
pub use builder::{MapKey, MapValue};

pub use helper::ops;
pub use helper::{
    AliasError, BPF_ANY, BPF_EXIST, BPF_NOEXIST, MAX_MAP_ALIASES, bind_map_symbols,
    clear_map_aliases, helper_delete, helper_for_each_map_elem, helper_lookup, helper_update,
    resolve_map_id, set_map_aliases, unbind_map_addr,
};
pub use helper::{MapError, MapOps};

pub use kinds::hash::{BpfHashTable, HashBuildError, KeyBlob, ValSlot};
#[cfg(feature = "ringbuf")]
pub use kinds::ringbuf;
#[cfg(feature = "ringbuf")]
pub use kinds::ringbuf::{
    MAX_RECORD_BYTES as RINGBUF_MAX_RECORD_BYTES, RINGBUF_SLOT_BYTES, RingbufRecord, RingbufRx,
    helper_ringbuf_discard, helper_ringbuf_output, helper_ringbuf_query, helper_ringbuf_reserve,
    helper_ringbuf_submit, ringbuf_record_payload,
};

#[cfg(feature = "ringbuf")]
pub use runtime::MapIdxExt;
pub use runtime::names::{register_map_name, resolve_map_name};
pub use runtime::pool::HEAP_SIZE;
pub use runtime::{
    Error, GuardError, MAP_RT, MapGuard, MapId, MapIdx, MapMeta, MapMutGuard, MapRt, MapRtGuard,
    RtMutex, RtMutexGuard,
};

/// Static bpf.toml map boot descriptor (codegen → firmware `BOOT_MAPS`).
#[derive(Clone, Copy, Debug)]
pub struct BootMapDesc {
    pub name: &'static str,
    pub map_type: MapType,
    pub key_layout: Option<&'static [u8]>,
    pub value_layout: Option<&'static [u8]>,
    pub len: usize,
    /// When true, skip create in [`MapBuilder::from_bpf_toml`].
    pub runtime: bool,
}

/// User-facing [`MapBuilder`] facade (`hash` / `array` / `ringbuf` entrypoints).
pub struct MapBuilder;

impl MapBuilder {
    pub fn hash() -> TypedMapBuilder<HashView, builder::Incomplete> {
        TypedMapBuilder::hash()
    }

    pub fn array() -> TypedMapBuilder<ArrayView, builder::Incomplete> {
        TypedMapBuilder::array()
    }

    #[cfg(feature = "ringbuf")]
    pub fn ringbuf() -> TypedMapBuilder<RingbufView, builder::Incomplete> {
        TypedMapBuilder::ringbuf()
    }

    /// Create maps from a complete boot descriptor table (path B).
    pub fn from_bpf_toml(descs: &[BootMapDesc], rt: &mut MapRt) -> Result<(), Error> {
        rt.create_from_boot_descs(descs)
    }
}

pub use xpost_macros::{MapKey, MapValue, declare_runtime};

/// Empty AOT binds when firmware does not include generated `aot_map_bind.rs`.
pub fn bind_aot_map_addrs() {}
pub const AOT_MAP_BIND_COUNT: usize = 0;
pub const AOT_MAP_BINDS: &[(&str, usize)] = &[];
pub const AOT_MAP_USER_NAMES: &[&str] = &[];

/// Stubs formerly emitted by board bpf_maps codegen (library no longer includes OUT_DIR).
pub const FIRMWARE_MAP_COUNT: usize = 0;
pub const FIRMWARE_HAS_ARRAY: bool = false;
pub const FIRMWARE_HAS_HASH: bool = false;
pub const FIRMWARE_HAS_RINGBUF: bool = false;
pub const SELFTEST_MAP_ID: MapIdx = 0;
pub const SELFTEST_MAP_TYPE: &str = "stub";
pub const SELFTEST_KEY_SIZE: usize = 4;
pub const SELFTEST_VALUE_SIZE: usize = 8;
