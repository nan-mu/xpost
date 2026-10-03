//! Lease-time AOT VMA → [`MapIdx`] alias table.

use core::cell::RefCell;

use critical_section::Mutex;
use portable_atomic::{AtomicUsize, Ordering};

use crate::map::{MapIdx, MapRt, MapType, kinds::array::BpfArrayMap, state::MAX_MAP_ALIASES};

type AliasSlot = Option<(usize, MapIdx)>;

/// Bumped under CS after alias table writes (weak publish token for readers).
static ALIAS_EPOCH: Mutex<RefCell<u32>> = Mutex::new(RefCell::new(0));
/// `(aot_addr, MapIdx)` slots; mutated only inside `critical_section`.
static MAP_ALIASES: Mutex<RefCell<[AliasSlot; MAX_MAP_ALIASES]>> =
    Mutex::new(RefCell::new([None; MAX_MAP_ALIASES]));

fn publish_aliases() {
    critical_section::with(|cs| {
        let mut e = ALIAS_EPOCH.borrow_ref_mut(cs);
        *e = e.wrapping_add(1);
    });
}

/// Replace the entire alias table (dynamic ABI install path).
///
/// `binds` entries with `addr == 0` are skipped. Fails if filtered length
/// exceeds [`MAX_MAP_ALIASES`].
pub(crate) fn set_map_aliases(binds: &[(usize, MapIdx)]) -> Result<(), AliasFull> {
    let filtered: heapless::Vec<(usize, MapIdx), MAX_MAP_ALIASES> = {
        let mut v = heapless::Vec::new();
        for &(addr, id) in binds {
            if addr == 0 {
                continue;
            }
            if v.push((addr, id)).is_err() {
                return Err(AliasFull);
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
pub(crate) fn clear_map_aliases() {
    critical_section::with(|cs| {
        *MAP_ALIASES.borrow_ref_mut(cs) = [None; MAX_MAP_ALIASES];
    });
    for f in &FAST_ARRAYS {
        f.alias.store(0, Ordering::Release);
    }
    publish_aliases();
}

/// Pre-resolved ARRAY alias for the XDP hot path: no MapRt lock, no flash code.
///
/// Written only while no flight can run: `XdpRt::lease` fills it before
/// `ACTIVE=true`; `XdpRt::release` clears it after `IN_FLIGHT==0`. Storage stays
/// valid meanwhile because the lease holds a map user.
struct FastArray {
    alias: AtomicUsize,
    base: AtomicUsize,
    stride: AtomicUsize,
    max_entries: AtomicUsize,
}

static FAST_ARRAYS: [FastArray; MAX_MAP_ALIASES] = [const {
    FastArray {
        alias: AtomicUsize::new(0),
        base: AtomicUsize::new(0),
        stride: AtomicUsize::new(0),
        max_entries: AtomicUsize::new(0),
    }
}; MAX_MAP_ALIASES];

/// Fill [`FAST_ARRAYS`] from lease binds (ARRAY maps only; others use the slow path).
pub(crate) fn set_fast_arrays(rt: &MapRt, binds: &[(usize, MapIdx)]) {
    let mut i = 0;
    for &(addr, id) in binds {
        let Some(meta) = rt.meta.get(id) else { continue };
        if addr == 0 || meta.storage.kind != Some(MapType::Array) || meta.storage.ptr.is_null() {
            continue;
        }
        let arr = unsafe { &*(meta.storage.ptr as *const BpfArrayMap) };
        let f = &FAST_ARRAYS[i];
        f.base.store(arr.elem_ptr(0) as usize, Ordering::Relaxed);
        f.stride.store(arr.value_stride, Ordering::Relaxed);
        f.max_entries.store(arr.max_entries, Ordering::Relaxed);
        f.alias.store(addr, Ordering::Release);
        i += 1;
    }
}

/// `Some(elem_ptr_or_0)` if `map` is a pre-resolved ARRAY alias; `None` → slow path.
#[esp_hal::ram]
pub(crate) fn fast_array_lookup(map: usize, key: usize) -> Option<usize> {
    for f in &FAST_ARRAYS {
        let a = f.alias.load(Ordering::Acquire);
        if a == 0 {
            return None;
        }
        if a != map {
            continue;
        }
        if key == 0 {
            return Some(0);
        }
        let idx = unsafe { core::ptr::read_unaligned(key as *const u32) } as usize;
        if idx >= f.max_entries.load(Ordering::Relaxed) {
            return Some(0);
        }
        let base = f.base.load(Ordering::Relaxed);
        return Some(base + idx * f.stride.load(Ordering::Relaxed));
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AliasFull;

impl From<AliasFull> for crate::error::XdpLeaseError {
    fn from(_: AliasFull) -> Self {
        Self::AliasFull
    }
}

/// Resolve helper `map` arg: AOT pointer alias only (fail-closed).
///
/// Unresolved VMAs return `None` — callers must not treat a bare integer as a
/// [`MapIdx`] fallback (that masked binding bugs on the hot path).
#[esp_hal::ram]
pub(crate) fn resolve_map_id(map: usize) -> Option<MapIdx> {
    if map == 0 {
        return None;
    }
    critical_section::with(|cs| {
        let _epoch = *ALIAS_EPOCH.borrow_ref(cs);
        let table = MAP_ALIASES.borrow_ref(cs);
        for slot in table.iter() {
            if let Some((a, id)) = *slot
                && a == map
            {
                return Some(id);
            }
        }
        None
    })
}
