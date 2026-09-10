//! XDP MAP resource pool + BPF map types + helper ABI.

pub mod builder;
pub mod helper;
pub mod kinds;
pub mod runtime;

#[cfg(feature = "event-ringbuf")]
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
#[cfg(feature = "event-ringbuf")]
pub use kinds::ringbuf;
#[cfg(feature = "event-ringbuf")]
pub use kinds::ringbuf::{
    MAX_RECORD_BYTES as RINGBUF_MAX_RECORD_BYTES, RINGBUF_SLOT_BYTES, RingbufRecord, RingbufRx,
    helper_ringbuf_discard, helper_ringbuf_output, helper_ringbuf_query, helper_ringbuf_reserve,
    helper_ringbuf_submit, ringbuf_record_payload,
};

#[cfg(feature = "event-ringbuf")]
pub use runtime::MapIdxExt;
#[cfg(feature = "from-bpf-toml")]
pub use runtime::boot::init_firmware_maps;
pub use runtime::names::{register_map_name, resolve_map_name};
pub use runtime::pool::HEAP_SIZE;
pub use runtime::{
    Error, GuardError, MAP_RT, MapGuard, MapId, MapIdx, MapMeta, MapMutGuard, MapRt, MapRtGuard,
    RtMutex, RtMutexGuard,
};

/// User-facing [`MapBuilder`] facade (`hash` / `array` / `ringbuf` entrypoints).
pub struct MapBuilder;

impl MapBuilder {
    pub fn hash() -> TypedMapBuilder<HashView, builder::Incomplete> {
        TypedMapBuilder::hash()
    }

    pub fn array() -> TypedMapBuilder<ArrayView, builder::Incomplete> {
        TypedMapBuilder::array()
    }

    #[cfg(feature = "event-ringbuf")]
    pub fn ringbuf() -> TypedMapBuilder<RingbufView, builder::Incomplete> {
        TypedMapBuilder::ringbuf()
    }

    #[cfg(feature = "from-bpf-toml")]
    pub async fn from_bpf_toml(rt: &mut MapRt) -> Result<(), Error> {
        rt.from_bpf_toml().await
    }
}

pub use xpost_macros::{MapKey, MapValue};

include!(concat!(env!("OUT_DIR"), "/bpf_maps.rs"));
include!(concat!(env!("OUT_DIR"), "/aot_map_bind.rs"));
