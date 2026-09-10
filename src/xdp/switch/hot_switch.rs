//! Multi-image hot switch (Cargo feature `hot-switch`).
//!
//! **Next version:** full hot_switch. This build only validates via `pre_lease`
//! and returns [`XdpError::NotImplemented`].

use crate::map::MapRtGuard;
use crate::xdp::XdpRt;
use crate::xdp::error::XdpError;
use crate::xdp::meta::XdpLocation;

impl XdpRt {
    /// Hot-switch to the program at `loc` (next version).
    ///
    /// Runs the same resource checks as [`Self::pre_lease`] against the
    /// candidate, then returns [`XdpError::NotImplemented`].
    pub fn hot_switch(
        &self,
        loc: XdpLocation,
        maps: Option<&MapRtGuard<'_>>,
    ) -> Result<(), XdpError> {
        if !self.leased {
            return Err(XdpError::NotLeased);
        }
        let id = match loc {
            XdpLocation::Slot { index } => {
                let slot = self.slots.get(index).ok_or(XdpError::UnknownSlot)?;
                let id = slot.image.ok_or(XdpError::HotSwitch)?;
                self.check(id, index)?;
                id
            }
            XdpLocation::Table { .. } => return Err(XdpError::HotSwitch),
        };
        let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
        image.meta.check_required_maps(maps)?;
        let _ = maps;
        Err(XdpError::NotImplemented)
    }
}
