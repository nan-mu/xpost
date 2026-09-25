//! Lease-time VMR→RMA bindings and MapRt user holdings.
//!
//! **VMR** = AOT `.maps` symbol virtual address (from XDP ELF meta `aot_binds`).
//! **RMA** = real map storage pointer from [`MapRt`].
//!
//! While leased, helpers resolve VMR via `set_map_aliases`. On release / rollback,
//! aliases are cleared and MapRt user refs are dropped.
//!
//! The table is fixed-capacity [`heapless::LinearMap`] — Talc is reserved for
//! bpf_map allocations inside `map_rt` only.

use crate::{
    map::{MapId, MapIdx, MapRtGuard},
    xdp::{
        XdpError, XdpRt,
        state::{MAX_MAP_REFS, MAX_VMR_BINDS},
    },
};

/// One published VMR→RMA binding for helpers during lease.
struct VmrBind {
    map_id: MapId,
    #[allow(dead_code)] // retained for lease-time RMA audit / future helper paths
    rma: usize,
}

/// VMR table keyed by AOT symbol VMA.
pub(crate) struct VmrTable {
    by_vma: heapless::LinearMap<usize, VmrBind, MAX_VMR_BINDS>,
}

impl VmrTable {
    pub(crate) fn new() -> Self {
        Self {
            by_vma: heapless::LinearMap::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.by_vma.clear();
    }

    pub(crate) fn insert(&mut self, vma: usize, map_id: MapId, rma: usize) -> Result<(), ()> {
        if self.by_vma.len() >= MAX_VMR_BINDS && !self.by_vma.contains_key(&vma) {
            return Err(());
        }
        match self.by_vma.insert(vma, VmrBind { map_id, rma }) {
            Ok(_) => Ok(()),
            Err(_) => Err(()),
        }
    }

    pub(crate) fn alias_pairs(&self) -> heapless::Vec<(usize, MapId), MAX_VMR_BINDS> {
        let mut out = heapless::Vec::new();
        for (&vma, b) in self.by_vma.iter() {
            let _ = out.push((vma, b.map_id));
        }
        out
    }
}

/// Lease-time map holdings: VMR→RMA table + MapRt user refs.
pub(crate) struct MapLease {
    pub(crate) vmr: VmrTable,
    pub(crate) refs: heapless::Vec<MapIdx, MAX_MAP_REFS>,
}

impl MapLease {
    pub(crate) fn new() -> Self {
        Self {
            vmr: VmrTable::new(),
            refs: heapless::Vec::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.vmr.clear();
        self.refs.clear();
    }
}

impl XdpRt {
    /// Resolve map name → add_user + insert VMR→RMA row.
    pub(crate) fn bind_vmr(
        &mut self,
        maps: &mut MapRtGuard<'_>,
        name: &str,
        vma: usize,
    ) -> Result<(), XdpError> {
        let map_id = maps.find(name).map_err(|_| XdpError::RequiredMapAbsent)?;
        let rma = maps.storage_ptr(map_id).map_err(|_| XdpError::MapLease)?;
        if !self.map_lease.refs.iter().any(|m| *m == map_id) {
            maps.add_user(map_id).map_err(|_| XdpError::MapLease)?;
            self.map_lease
                .refs
                .push(map_id)
                .map_err(|_| XdpError::MapLease)?;
        }
        self.map_lease
            .vmr
            .insert(vma, map_id, rma)
            .map_err(|_| XdpError::MapLease)?;
        Ok(())
    }

    /// Drop all lease map users, clear VMR table and helper aliases.
    pub(crate) fn rollback_map_users(&mut self, maps: &mut MapRtGuard<'_>) {
        for id in self.map_lease.refs.drain(..) {
            let _ = maps.remove_user(id);
        }
        self.map_lease.vmr.clear();
        crate::map::clear_map_aliases();
    }
}
