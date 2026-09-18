//! XDP MAP resource pool + BPF map types + helper ABI.

pub(crate) mod builder;
pub(crate) mod helper;
pub(crate) mod kinds;
pub(crate) mod runtime;

pub use builder::{MapBuilder, MapKey, MapType, MapValue};

pub use helper::{MapError, MapOps, bind_map_symbols};
/// Re-export map ops under the historical `map::ops` path.
pub mod ops {
    pub use crate::map::helper::ops::{MapError, MapOps};
}

pub(crate) use helper::{
    BPF_ANY, clear_map_aliases, helper_delete, helper_lookup, helper_update, set_map_aliases,
};

#[cfg(feature = "ringbuf")]
pub(crate) use kinds::ringbuf::{
    MAX_RECORD_BYTES as RINGBUF_MAX_RECORD_BYTES, RINGBUF_SLOT_BYTES, RingbufRecord,
    helper_ringbuf_discard, helper_ringbuf_output, helper_ringbuf_query, helper_ringbuf_reserve,
    helper_ringbuf_submit,
};
#[cfg(feature = "ringbuf")]
pub use kinds::ringbuf::{RingbufRx, ringbuf_record_payload};

#[cfg(feature = "ringbuf")]
pub use runtime::MapIdxExt;
pub use runtime::names::{register_map_name, resolve_map_name};
pub use runtime::pool::HEAP_SIZE;
pub use runtime::{
    Error, GuardError, MAP_RT, MapGuard, MapId, MapIdx, MapMeta, MapMutGuard, MapRt, MapRtGuard,
    RtMutex, RtMutexGuard,
};

pub use xpost_macros::{MapKey, MapValue, declare_runtime};

/// Empty AOT binds when firmware does not include generated `aot_map_bind.rs`.
pub fn bind_aot_map_addrs() {}
pub const AOT_MAP_BIND_COUNT: usize = 0;
pub const AOT_MAP_BINDS: &[(&str, usize)] = &[];
pub const AOT_MAP_USER_NAMES: &[&str] = &[];

/// Stubs formerly emitted by board bpf_maps codegen (library no longer includes OUT_DIR).
#[allow(dead_code)]
pub(crate) const FIRMWARE_MAP_COUNT: usize = 0;
#[allow(dead_code)]
pub(crate) const FIRMWARE_HAS_ARRAY: bool = false;
#[allow(dead_code)]
pub(crate) const FIRMWARE_HAS_HASH: bool = false;
#[allow(dead_code)]
pub(crate) const FIRMWARE_HAS_RINGBUF: bool = false;
#[allow(dead_code)]
pub(crate) const SELFTEST_MAP_ID: MapIdx = 0;
#[allow(dead_code)]
pub(crate) const SELFTEST_MAP_TYPE: &str = "stub";
#[allow(dead_code)]
pub(crate) const SELFTEST_KEY_SIZE: usize = 4;
#[allow(dead_code)]
pub(crate) const SELFTEST_VALUE_SIZE: usize = 8;
