//! XDP MAP resource pool + BPF map types + helper ABI.
//!
//! Layout:
//! - [`pool`] — talc heap + Id→[`RawResource`] table (not MCU “register”)
//! - [`abi`] — AOT map aliases + sync helper entrypoints
//! - [`array`] / [`hash`] / [`ringbuf`] — concrete map types

pub mod abi;
pub mod array;
pub mod ctrl;
pub mod hash;
pub mod names;
pub mod pool;
pub mod ringbuf;

pub use abi::{
    AliasError, BPF_ANY, BPF_EXIST, BPF_NOEXIST, MAX_MAP_ALIASES, clear_map_aliases, helper_delete,
    helper_for_each_map_elem, helper_lookup, helper_update, register_map_addr, resolve_map_id,
    set_map_aliases, unbind_map_addr,
};
pub use array::{BpfArrayMap, create_array, create_bpf_array, create_bpf_devmap};
pub use ctrl::{MAX_MAP_IDS, MapCreateCmd, MapCtrlError, create_map, destroy_map};
pub use hash::{BpfHashMap, KeyBlob, SharedHashMap, ValSlot, create_bpf_hash, create_hashmap};
pub use names::{register_map_name, resolve_map_name};
pub use pool::{MapKind, RawResource, ResourceId, TalcAlloc, allocator, init};
pub use ringbuf::{
    BpfRingBuf, TalcRingBuf, TalcVec, create_bpf_ringbuf, create_ringbuf, helper_ringbuf_discard,
    helper_ringbuf_discard_dynptr, helper_ringbuf_output, helper_ringbuf_query,
    helper_ringbuf_reserve, helper_ringbuf_reserve_dynptr, helper_ringbuf_submit,
    helper_ringbuf_submit_dynptr,
};

include!(concat!(env!("OUT_DIR"), "/bpf_maps.rs"));
include!(concat!(env!("OUT_DIR"), "/aot_map_bind.rs"));

pub mod userspace;
pub use userspace::{hash_insert, with_hash_map};
