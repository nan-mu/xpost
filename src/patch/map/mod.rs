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
pub mod pool;
pub mod ringbuf;

pub use abi::{
    clear_map_aliases, helper_delete, helper_for_each_map_elem, helper_lookup, helper_update,
    register_map_addr, resolve_map_id, set_map_aliases, unbind_map_addr, AliasError, BPF_ANY,
    BPF_EXIST, BPF_NOEXIST, MAX_MAP_ALIASES,
};
pub use array::{create_array, create_bpf_array, BpfArrayMap};
pub use ctrl::{create_map, destroy_map, MapCreateCmd, MapCtrlError, MAX_MAP_IDS};
pub use hash::{create_bpf_hash, create_hashmap, BpfHashMap, SharedHashMap};
pub use pool::{allocator, init, MapKind, RawResource, ResourceId, TalcAlloc};
pub use ringbuf::{
    create_bpf_ringbuf, create_ringbuf, helper_ringbuf_discard, helper_ringbuf_discard_dynptr,
    helper_ringbuf_output, helper_ringbuf_query, helper_ringbuf_reserve,
    helper_ringbuf_reserve_dynptr, helper_ringbuf_submit, helper_ringbuf_submit_dynptr, BpfRingBuf,
    TalcRingBuf, TalcVec,
};

include!(concat!(env!("OUT_DIR"), "/bpf_maps.rs"));
include!(concat!(env!("OUT_DIR"), "/aot_map_bind.rs"));
