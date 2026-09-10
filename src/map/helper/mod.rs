//! Sync BPF map helper ABI (AOT `map` arg → pool Id → concrete ops).
//!
//! # Safety subset (ARRAY / HASH)
//!
//! BPF helpers [`helper_lookup`] / [`helper_update`] / [`helper_delete`] accept
//! only [`MapKind::Array`] and [`MapKind::Hash`]. Other kinds return
//! [`MapError::Unsupported`].
//!
//! - **ARRAY**: lookup / update; delete → Unsupported (Linux-aligned).
//! - **HASH**: lookup / update / delete; flags `BPF_ANY|NOEXIST|EXIST`.
//! - **lookup null**: miss and pool Busy both return `0` (indistinguishable).
//! - **value pointer**: valid for the rest of the current flight while the map
//!   remains leased; delete (HASH) invalidates prior lookup pointers.
//! - **for_each_map_elem**: unsupported (see [`helper_for_each_map_elem`]).
//!
//! # Alias table (weak sync)
//!
//! Writers update [`MAP_ALIASES`] under `critical_section` then bump
//! [`ALIAS_EPOCH`] (`Release`). Readers (`resolve_map_id`) load the epoch with
//! `Acquire` before scanning — single-core MCU: no full lock on the XDP path,
//! just publish ordering so a torn update is not observed mid-helper.

use core::cell::RefCell;

use critical_section::Mutex;

pub mod ops;

use crate::map::kinds::array::BpfArrayMap;
use crate::map::kinds::hash::BpfHashMap;
#[cfg(feature = "event-ringbuf")]
use crate::map::kinds::ringbuf::RingbufMap;
use crate::map::runtime::pool::{MapKind, ResourceId, pool_try_read, pool_try_write};
pub use ops::{MapError, MapOps};

/// Linux `bpf_map_update_elem` flags (low bits).
pub const BPF_ANY: u64 = 0;
pub const BPF_NOEXIST: u64 = 1;
pub const BPF_EXIST: u64 = 2;

pub const MAX_MAP_ALIASES: usize = 16;

/// Bumped under CS after alias table writes (weak publish token for readers).
static ALIAS_EPOCH: Mutex<RefCell<u32>> = Mutex::new(RefCell::new(0));
/// `(aot_addr, ResourceId)` slots; mutated only inside `critical_section`.
static MAP_ALIASES: Mutex<RefCell<[Option<(usize, ResourceId)>; MAX_MAP_ALIASES]>> =
    Mutex::new(RefCell::new([None; MAX_MAP_ALIASES]));

fn publish_aliases() {
    critical_section::with(|cs| {
        let mut e = ALIAS_EPOCH.borrow_ref_mut(cs);
        *e = e.wrapping_add(1);
    });
}

/// Bind AOT `.maps` symbol VMAs to maps already registered by name.
///
/// `binds` are `(symbol_name, vma)` from the MCU link report (`obj_report`).
/// Unknown names and `addr == 0` are skipped. Called when an XDP image loads —
/// not from embedder map registration.
pub fn bind_map_symbols(binds: &[(&str, usize)]) -> usize {
    let mut n = 0usize;
    for &(name, addr) in binds {
        if addr == 0 {
            continue;
        }
        let Some(id) = crate::map::runtime::names::resolve_map_name(name) else {
            log::warn!("[MAP] bind_map_symbols: unknown name {name}");
            continue;
        };
        if register_map_addr(addr, id) {
            n += 1;
        }
    }
    n
}

/// Bind an AOT `.maps` object address to a firmware [`ResourceId`].
///
/// Prefer [`bind_map_symbols`] at XDP load; this is the low-level alias slot writer.
///
/// Returns `false` if the alias table is full.
pub(crate) fn register_map_addr(addr: usize, id: ResourceId) -> bool {
    if addr == 0 {
        return true;
    }
    let ok = critical_section::with(|cs| {
        let mut table = MAP_ALIASES.borrow_ref_mut(cs);
        for slot in table.iter_mut() {
            if let Some((a, _)) = *slot {
                if a == addr {
                    *slot = Some((addr, id));
                    return true;
                }
            }
        }
        for slot in table.iter_mut() {
            if slot.is_none() {
                *slot = Some((addr, id));
                return true;
            }
        }
        false
    });
    if ok {
        publish_aliases();
    }
    ok
}

/// Remove a single AOT address binding (no-op if absent).
pub fn unbind_map_addr(addr: usize) {
    if addr == 0 {
        return;
    }
    critical_section::with(|cs| {
        let mut table = MAP_ALIASES.borrow_ref_mut(cs);
        for slot in table.iter_mut() {
            if matches!(*slot, Some((a, _)) if a == addr) {
                *slot = None;
            }
        }
    });
    publish_aliases();
}

/// Replace the entire alias table (dynamic ABI install path).
///
/// `binds` entries with `addr == 0` are skipped. Fails if filtered length
/// exceeds [`MAX_MAP_ALIASES`].
pub fn set_map_aliases(binds: &[(usize, ResourceId)]) -> Result<(), AliasError> {
    let filtered: heapless::Vec<(usize, ResourceId), MAX_MAP_ALIASES> = {
        let mut v = heapless::Vec::new();
        for &(addr, id) in binds {
            if addr == 0 {
                continue;
            }
            if v.push((addr, id)).is_err() {
                return Err(AliasError::Full);
            }
        }
        v
    };

    critical_section::with(|cs| {
        let mut table = MAP_ALIASES.borrow_ref_mut(cs);
        *table = [None; MAX_MAP_ALIASES];
        for (i, &(addr, id)) in filtered.iter().enumerate() {
            table[i] = Some((addr, id));
        }
    });
    publish_aliases();
    Ok(())
}

/// Clear every alias (e.g. before a full ABI reload).
pub fn clear_map_aliases() {
    critical_section::with(|cs| {
        *MAP_ALIASES.borrow_ref_mut(cs) = [None; MAX_MAP_ALIASES];
    });
    publish_aliases();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasError {
    Full,
}

/// Resolve helper `map` arg: AOT pointer alias only (fail-closed).
///
/// Unresolved VMAs return `None` — callers must not treat a bare integer as a
/// [`ResourceId`] fallback (that masked binding bugs on the hot path).
pub fn resolve_map_id(map: usize) -> Option<ResourceId> {
    if map == 0 {
        return None;
    }
    critical_section::with(|cs| {
        // Touch epoch so reads are sequenced after the last publish (weak sync).
        let _epoch = *ALIAS_EPOCH.borrow_ref(cs);
        let table = MAP_ALIASES.borrow_ref(cs);
        for slot in table.iter() {
            if let Some((a, id)) = *slot {
                if a == map {
                    return Some(id);
                }
            }
        }
        None
    })
}

/// Registry entry viewed as a concrete BPF map (exclusive / write path).
pub(crate) enum TypedMap<'a> {
    Array(&'a mut BpfArrayMap),
    Hash(&'a mut BpfHashMap),
    #[cfg(feature = "event-ringbuf")]
    Ringbuf(&'a mut RingbufMap),
}

pub(crate) enum TypedMapRef<'a> {
    Array(&'a BpfArrayMap),
    Hash(&'a BpfHashMap),
    #[cfg(feature = "event-ringbuf")]
    Ringbuf(&'a RingbufMap),
}

pub(crate) fn with_typed_map<R>(id: ResourceId, f: impl FnOnce(TypedMap<'_>) -> R) -> Option<R> {
    let mut guard = pool_try_write()?;
    let mut table = guard.as_mut()?;
    let mut res = table.get_mut(&id)?;
    let typed = match res.kind()? {
        MapKind::Array => TypedMap::Array(unsafe { res.as_mut() }),
        MapKind::Hash => TypedMap::Hash(unsafe { res.as_mut() }),
        #[cfg(feature = "event-ringbuf")]
        MapKind::Ringbuf => TypedMap::Ringbuf(unsafe { res.as_mut() }),
    };
    Some(f(typed))
}

pub(crate) fn with_typed_map_ref<R>(
    id: ResourceId,
    f: impl FnOnce(TypedMapRef<'_>) -> R,
) -> Option<R> {
    let guard = pool_try_read()?;
    let table = guard.as_ref()?;
    let res = table.get(&id)?;
    let typed = match res.kind()? {
        MapKind::Array => TypedMapRef::Array(unsafe { res.as_ref() }),
        MapKind::Hash => TypedMapRef::Hash(unsafe { res.as_ref() }),
        #[cfg(feature = "event-ringbuf")]
        MapKind::Ringbuf => TypedMapRef::Ringbuf(unsafe { res.as_ref() }),
    };
    Some(f(typed))
}

/// Sync BPF helper entry: resolve `map` as AOT VMA alias only (fail-closed).
///
/// ARRAY/HASH only. Uses a pool **read** lock so lookups do not exclusive-contend.
/// `Busy` (writer held) still returns null — same ABI as a key miss.
pub fn helper_lookup(map: usize, key: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        log::error!("[Helper][MAP][helper_lookup]: unresolved map ptr {}", map);
        return 0;
    };
    let key = key as *const u8;
    match with_typed_map_ref(id, |typed| match typed {
        TypedMapRef::Array(m) => MapOps::lookup(m, key),
        TypedMapRef::Hash(m) => MapOps::lookup(m, key),
        #[cfg(feature = "event-ringbuf")]
        TypedMapRef::Ringbuf(_) => Err(MapError::Unsupported),
    }) {
        Some(Ok(p)) => p as usize,
        Some(Err(MapError::NotFound)) => 0,
        Some(Err(MapError::Unsupported)) => {
            MapError::Unsupported.log_if_bug("helper_lookup");
            0
        }
        Some(Err(_)) => 0,
        None => {
            MapError::Busy.log_if_bug("helper_lookup");
            0
        }
    }
}

pub fn helper_update(map: usize, key: usize, value: usize, flags: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return MapError::InvalidArg.errno() as usize;
    };
    let key = key as *const u8;
    let value = value as *const u8;
    match with_typed_map(id, |typed| match typed {
        TypedMap::Array(m) => MapOps::update(m, key, value, flags as u64),
        TypedMap::Hash(m) => MapOps::update(m, key, value, flags as u64),
        #[cfg(feature = "event-ringbuf")]
        TypedMap::Ringbuf(_) => Err(MapError::Unsupported),
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            e.log_if_bug("helper_update");
            e.errno() as usize
        }
        None => {
            MapError::Busy.log_if_bug("helper_update");
            MapError::Busy.errno() as usize
        }
    }
}

pub fn helper_delete(map: usize, key: usize) -> usize {
    let Some(id) = resolve_map_id(map) else {
        return MapError::InvalidArg.errno() as usize;
    };
    let key = key as *const u8;
    match with_typed_map(id, |typed| match typed {
        TypedMap::Array(m) => MapOps::delete(m, key),
        TypedMap::Hash(m) => MapOps::delete(m, key),
        #[cfg(feature = "event-ringbuf")]
        TypedMap::Ringbuf(_) => Err(MapError::Unsupported),
    }) {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            e.log_if_bug("helper_delete");
            e.errno() as usize
        }
        None => {
            MapError::Busy.log_if_bug("helper_delete");
            MapError::Busy.errno() as usize
        }
    }
}

type ForEachCb = extern "C" fn(usize, usize, usize, usize) -> i32;

/// `bpf_for_each_map_elem` is **unsupported** in the safety-trimmed runtime.
///
/// Blocking reasons (N1 / N2):
/// - callback is an unchecked function pointer (`transmute`);
/// - PREVAIL does not analyze callback bodies;
/// - HASH snapshot may silently truncate;
/// - nested helper reentrancy / stack peak are unbudgeted.
///
/// Returns `-ENOTSUP`. Implementation retained under `allow(dead_code)` for a
/// future constrained restore — do not re-enable without closing SC-10.
#[allow(dead_code)]
pub fn helper_for_each_map_elem(
    map: usize,
    callback_fn: usize,
    callback_ctx: usize,
    _flags: usize,
) -> usize {
    let _ = (map, callback_fn, callback_ctx);
    let _cb: Option<ForEachCb> = None;
    (-95i32) as usize
}
