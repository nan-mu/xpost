//! XDP image metadata and runtime checks.

use alloc::string::String;

use crate::{
    map::MapMeta,
    xdp::{
        SlotId, XdpId,
        ready::ReadyError,
        slot::XdpSlot,
        state::{MAX_IMAGES, MAX_REQUIRED_MAPS},
    },
};

/// Program's current unique copy location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpLocation {
    /// Cold table entry index (not a byte offset).
    Table { index: XdpId },
    /// Executable slot index.
    Slot { index: SlotId },
}

/// Runtime image record — only what lease/move/bind need after load validation.
///
/// Wire-only fields (ELF header constants, xpost_version, machine, shoff, …) are
/// checked during Compact ELF scan and **not** retained here.
#[derive(Clone)]
pub(crate) struct XdpMeta {
    pub(crate) name: String,
    /// Hot-slot link / IMAGE `sh_addr`.
    pub(crate) link_addr: usize,
    pub(crate) helper_address: usize,
    pub(crate) required_maps: heapless::Vec<MapMeta, MAX_REQUIRED_MAPS>,
    /// Map name → linked `.maps` symbol VMA (`map_vma` from Compact ELF).
    /// AOT code loads maps by this absolute address; lease installs VMR→RMA aliases.
    pub(crate) aot_binds: heapless::Vec<(String, usize), MAX_REQUIRED_MAPS>,
    /// SHA-256 of the raw IMAGE bytes (optional integrity token).
    pub(crate) image_digest: [u8; 32],
    /// Absolute cold-region offset of the IMAGE payload.
    pub(crate) image_abs: usize,
    pub(crate) image_len: usize,
    /// Absolute cold-region offset of the Compact ELF container (for erase/scan).
    pub(crate) container_abs: usize,
    pub(crate) container_len: usize,
    pub(crate) location: XdpLocation,
}

/// Bounded list of metas from a cold-region scan.
pub(crate) type CollectedMetas = heapless::Vec<XdpMeta, MAX_IMAGES>;

impl XdpMeta {
    pub(crate) fn set_location(&mut self, location: XdpLocation) {
        self.location = location;
    }

    pub(crate) fn location(&self) -> XdpLocation {
        self.location
    }

    /// Absolute flash/table address of the executable image bytes.
    pub(crate) fn aot_abs(&self) -> usize {
        self.image_abs
    }

    pub(crate) fn check_arch(&self) -> Result<(), ReadyError> {
        if cfg!(target_feature = "c") {
            Ok(())
        } else {
            Err(ReadyError::Arch)
        }
    }

    pub(crate) fn check_helper_address(&self) -> Result<(), ReadyError> {
        if self.helper_address == 0 {
            return Err(ReadyError::HelperAddress);
        }
        let expected = crate::helper::helper_dispatcher as *const () as usize;
        if expected != 0 && self.helper_address != expected {
            return Err(ReadyError::HelperAddressMismatch);
        }
        Ok(())
    }

    pub(crate) fn check_slot_address(&self, slot: &XdpSlot) -> Result<(), ReadyError> {
        if self.link_addr != 0 && slot.base() != self.link_addr {
            return Err(ReadyError::SlotAddress);
        }
        if self.link_addr != 0 && (self.link_addr & 3) != 0 {
            return Err(ReadyError::SlotAddressAlign);
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn check_digest(&self, expected: Option<&[u8; 32]>) -> bool {
        let Some(exp) = expected else {
            return true;
        };
        if self.image_digest == [0; 32] {
            return true;
        }
        &self.image_digest == exp
    }
}
