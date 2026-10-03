//! Multi-image hot switch (Cargo feature `hot-switch`).
//!
//! First-case H′ uses a fixed primary image only. Enabling this feature
//! restores `save` / `switch` / `hot_switch` on [`super::XdpRt`].
//!
//! **Open safety constraint:** a flight that already captured `PRIMARY_ENTRY`
//! must keep executing that same N until it returns; overwriting the inactive
//! slot or map refs of an in-flight generation is still unverified. See
//! `build/session-SC-09-hot-switch-flight-invariance.md`.

use core::sync::atomic::Ordering;

use super::error::XdpError;
use super::meta;
use super::state::PRIMARY_ENTRY;
use super::storage;
use super::{XdpId, XdpRt};

impl XdpRt {
    /// Copy a non-primary hot slot back into cold storage.
    pub fn save(&mut self, id: XdpId) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        let (location, table_id, image_len) = {
            let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
            (image.meta.location(), image.table_id, image.image_len)
        };
        let meta::XdpLocation::Slot(source) = location else {
            return Ok(());
        };
        if source == self.primary {
            return Err(XdpError::Leased);
        }
        let (src, loaded_len, slot_image) = {
            let slot = self.slots.get(source).ok_or(XdpError::UnknownSlot)?;
            (slot.slot.base() as *const u8, slot.loaded_len, slot.image)
        };
        if slot_image != Some(id) || loaded_len != image_len {
            return Err(XdpError::MetaDisagree);
        }
        let mut buf = [0u8; storage::DEFAULT_IMAGE_CAPACITY];
        if loaded_len > buf.len() {
            return Err(XdpError::ScratchTooSmall);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), loaded_len);
        }
        let offset = table_id * self.table.image_capacity();
        self.table.erase(offset, self.table.image_capacity());
        self.table.write(offset, &buf[..loaded_len]);
        let slot = self.slots.get_mut(source).ok_or(XdpError::UnknownSlot)?;
        slot.loaded_len = 0;
        slot.image = None;
        self.images
            .get_mut(id)
            .ok_or(XdpError::UnknownImage)?
            .meta
            .set_location(meta::XdpLocation::Table(table_id));
        Ok(())
    }

    /// Load `id` into its target slot and make that slot primary (not leased).
    pub fn switch(&mut self, id: XdpId) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        let addr = self
            .images
            .get(id)
            .ok_or(XdpError::UnknownImage)?
            .meta
            .slot_address();
        let target = if addr == 0 {
            self.primary
        } else {
            self.slot_for(addr)?
        };
        self.load(id, target)?;
        self.primary = target;
        Ok(())
    }

    /// Publish a candidate already resident in an inactive slot while leased.
    ///
    /// Does not wait for `IN_FLIGHT`; the next RX flight observes the new entry.
    /// Flight-period invariance of the *previous* entry is not yet enforced here.
    pub fn hot_switch(&mut self, id: XdpId) -> Result<(), XdpError> {
        if !self.leased {
            return Err(XdpError::NotLeased);
        }
        let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
        let meta::XdpLocation::Slot(target) = image.meta.location() else {
            return Err(XdpError::HotSwitch);
        };
        if target == self.primary {
            return Err(XdpError::HotSwitch);
        }
        let slot = self.slots.get(target).ok_or(XdpError::UnknownSlot)?;
        if slot.image != Some(id) || slot.loaded_len == 0 {
            return Err(XdpError::HotSwitch);
        }
        PRIMARY_ENTRY.store(slot.slot.base(), Ordering::Release);
        self.primary = target;
        Ok(())
    }
}
