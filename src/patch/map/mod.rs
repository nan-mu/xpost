//! XDP MAP resource pool + BPF map types + helper ABI.
//!
//! Layout:
//! - [`pool`] — talc heap + Id→[`RawResource`] table (not MCU “register”)
//! - [`abi`] — AOT map aliases + sync helper entrypoints
//! - [`array`] / [`hash`] / [`ringbuf`] — concrete map types
//! - [`ops`] — XDP `MapOps` + [`MapError`]
//! - [`userspace`] — embassy write-lock native-container access

pub mod abi;
pub mod array;
pub mod ctrl;
pub mod hash;
pub mod names;
pub mod ops;
pub mod pool;
pub mod ringbuf;

pub use abi::{
    AliasError, BPF_ANY, BPF_EXIST, BPF_NOEXIST, MAX_MAP_ALIASES, clear_map_aliases, helper_delete,
    helper_for_each_map_elem, helper_lookup, helper_update, register_map_addr, resolve_map_id,
    set_map_aliases, unbind_map_addr,
};
pub use array::{BpfArrayMap, create_bpf_array, create_bpf_devmap, create_bpf_prog_array};
pub use ctrl::{MAX_MAP_IDS, MapCreateCmd, MapCtrlError, create_map, destroy_map};
pub use hash::{
    BpfHashMap, BpfHashMapBuilder, BpfHashTable, HashBuildError, KeyBlob, ValSlot, create_bpf_hash,
};
pub use names::{register_map_name, resolve_map_name};
pub use ops::MapError;
pub use pool::{MapKind, RawResource, ResourceId, TalcAlloc, allocator, init};
pub use ringbuf::{
    BpfRingBuf, create_bpf_ringbuf, helper_ringbuf_discard, helper_ringbuf_discard_dynptr,
    helper_ringbuf_output, helper_ringbuf_query, helper_ringbuf_reserve,
    helper_ringbuf_reserve_dynptr, helper_ringbuf_submit, helper_ringbuf_submit_dynptr,
};

include!(concat!(env!("OUT_DIR"), "/bpf_maps.rs"));
include!(concat!(env!("OUT_DIR"), "/aot_map_bind.rs"));

pub mod userspace;
pub use userspace::{with_array_map, with_hash_map, with_ringbuf_map};
