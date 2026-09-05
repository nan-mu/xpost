//! XDP MAP resource pool + BPF map types + helper ABI.

pub mod abi;
pub mod array;
pub mod boot;
pub mod builder;
pub mod ctrl;
pub mod hash;
pub mod names;
pub mod ops;
pub mod pool;
pub mod ringbuf;
pub mod runtime;
pub mod traits;
pub mod wdt_feed;

pub use abi::{
    AliasError, BPF_ANY, BPF_EXIST, BPF_NOEXIST, MAX_MAP_ALIASES, bind_map_symbols,
    clear_map_aliases, helper_delete, helper_for_each_map_elem, helper_lookup, helper_update,
    resolve_map_id, set_map_aliases, unbind_map_addr,
};
pub use boot::init_firmware_maps;
pub use builder::{
    ArrayView, CompleteBuilder, HashView, MapBuilder as TypedMapBuilder, MapType, RingbufView,
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

    pub fn ringbuf() -> TypedMapBuilder<RingbufView, builder::Incomplete> {
        TypedMapBuilder::ringbuf()
    }

    pub async fn from_bpf_toml(rt: &mut MapRt) -> Result<(), Error> {
        CompleteBuilder::from_bpf_toml(rt).await
    }
}

pub use array::{BpfArrayMap, create_bpf_array, create_bpf_devmap, create_bpf_prog_array};
pub use async_ringbuf;
pub use ctrl::{
    MAX_MAP_IDS, MapCreateCmd, MapCtrlError, create_map, destroy_map, register_map,
    register_ringbuf,
};
pub use hash::{
    BpfHashMap, BpfHashMapBuilder, BpfHashTable, HashBuildError, KeyBlob, ValSlot, create_bpf_hash,
};
pub use names::{register_map_name, resolve_map_name};
pub use ops::{MapError, MapOps};
pub use pool::{HEAP_SIZE, MapKind, RawResource, ResourceId, TalcAlloc, allocator};
pub use ringbuf::{
    BpfRingBuf, MAX_RECORD_BYTES as RINGBUF_MAX_RECORD_BYTES, RINGBUF_SLOT_BYTES, RingbufMap,
    RingbufRecord, RingbufRx, create_bpf_ringbuf, helper_ringbuf_discard,
    helper_ringbuf_discard_dynptr, helper_ringbuf_output, helper_ringbuf_query,
    helper_ringbuf_reserve, helper_ringbuf_reserve_dynptr, helper_ringbuf_submit,
    helper_ringbuf_submit_dynptr, ringbuf_record_payload,
};
pub use runtime::{
    Error, GuardError, MAP_RT, MapGuard, MapIdx, MapIdxExt, MapMeta, MapMutGuard, MapRt,
    MapRtReadGuard, MapRtWriteGuard, claim_heap, init, map_rt, map_rt_read,
};
pub use traits::{MapKey, MapValue};
pub use wdt_feed::{BpfWdtFeedMap, create_bpf_wdt_feed, helper_feed as helper_wdt_feed};

include!(concat!(env!("OUT_DIR"), "/bpf_maps.rs"));
include!(concat!(env!("OUT_DIR"), "/aot_map_bind.rs"));

pub mod userspace;
pub use userspace::{peek_array_u64, with_array_map, with_hash_map, with_ringbuf_map};

#[deprecated(note = "use claim_heap")]
pub use pool::init as init_pool_legacy;
