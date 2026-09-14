//! VMR (AOT `.maps` symbol address) → RMA bindings while XDP is leased.

use hashbrown::HashMap;

use crate::{
    map::{MapId, runtime::pool::TalcAlloc},
    xdp::state::MAX_VMR_BINDS,
};

/// One published VMR→RMA binding for helpers during lease.
struct VmrBind {
    map_id: MapId,
    #[allow(dead_code)] // retained for lease-time RMA audit / future helper paths
    rma: usize,
}

/// VMR table keyed by AOT symbol VMA.
pub(crate) struct VmrTable {
    by_vma: HashMap<usize, VmrBind, hashbrown::DefaultHashBuilder, TalcAlloc>,
}

impl VmrTable {
    pub(crate) fn new() -> Self {
        Self {
            by_vma: HashMap::new_in(TalcAlloc),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.by_vma.clear();
    }

    pub(crate) fn insert(&mut self, vma: usize, map_id: MapId, rma: usize) -> Result<(), ()> {
        if self.by_vma.len() >= MAX_VMR_BINDS && !self.by_vma.contains_key(&vma) {
            return Err(());
        }
        self.by_vma.insert(vma, VmrBind { map_id, rma });
        Ok(())
    }

    pub(crate) fn alias_pairs(&self) -> alloc::vec::Vec<(usize, MapId)> {
        self.by_vma
            .iter()
            .map(|(&vma, b)| (vma, b.map_id))
            .collect()
    }
}
