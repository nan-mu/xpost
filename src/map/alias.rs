//! Lease-time AOT VMA → [`MapIdx`] alias table.

use core::cell::RefCell;

use critical_section::Mutex;

use crate::map::{MapIdx, state::MAX_MAP_ALIASES};

/// Bumped under CS after alias table writes (weak publish token for readers).
static ALIAS_EPOCH: Mutex<RefCell<u32>> = Mutex::new(RefCell::new(0));
/// `(aot_addr, MapIdx)` slots; mutated only inside `critical_section`.
static MAP_ALIASES: Mutex<RefCell<[Option<(usize, MapIdx)>; MAX_MAP_ALIASES]>> =
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
    publish_aliases();
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
pub(crate) fn resolve_map_id(map: usize) -> Option<MapIdx> {
    if map == 0 {
        return None;
    }
    critical_section::with(|cs| {
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
