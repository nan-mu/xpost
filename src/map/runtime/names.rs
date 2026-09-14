//! Static bpf.toml map-name → [`MapIdx`] aliases for userspace lookup.

use core::cell::RefCell;

use critical_section::Mutex;

use crate::map::runtime::rt::MapIdx;

const MAX_MAP_NAMES: usize = 16;

static NAME_ALIASES: Mutex<RefCell<[Option<(&'static str, MapIdx)>; MAX_MAP_NAMES]>> =
    Mutex::new(RefCell::new([None; MAX_MAP_NAMES]));

/// Bind a bpf.toml map name to a pool [`MapIdx`].
///
/// Returns `false` if the table is full or `name` is empty.
pub fn register_map_name(name: &'static str, id: MapIdx) -> bool {
    if name.is_empty() {
        return false;
    }
    critical_section::with(|cs| {
        let mut table = NAME_ALIASES.borrow_ref_mut(cs);
        for slot in table.iter_mut() {
            if let Some((n, _)) = *slot {
                if n == name {
                    *slot = Some((name, id));
                    return true;
                }
            }
        }
        for slot in table.iter_mut() {
            if slot.is_none() {
                *slot = Some((name, id));
                return true;
            }
        }
        false
    })
}

/// Resolve a registered map name to its [`MapIdx`].
pub fn resolve_map_name(name: &str) -> Option<MapIdx> {
    critical_section::with(|cs| {
        let table = NAME_ALIASES.borrow_ref(cs);
        for slot in table.iter() {
            if let Some((n, id)) = *slot {
                if n == name {
                    return Some(id);
                }
            }
        }
        None
    })
}

/// Register name → idx (heap `String` names use leaked static for lookup table).
pub(crate) fn register_map_name_idx(name: &str, id: MapIdx) -> bool {
    let leaked: &'static str = alloc::borrow::ToOwned::to_owned(name).leak();
    register_map_name(leaked, id)
}

pub(crate) fn unregister_map_name(name: &str) {
    critical_section::with(|cs| {
        let mut table = NAME_ALIASES.borrow_ref_mut(cs);
        for slot in table.iter_mut() {
            if let Some((n, _)) = *slot {
                if n == name {
                    *slot = None;
                }
            }
        }
    });
}
