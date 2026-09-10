//! XDP image metadata and runtime checks.

use alloc::string::String;
use alloc::vec::Vec;

use super::error::XdpError;
use super::slot::XdpSlot;
use crate::map::MapRtGuard;
use crate::xdp::{XdpSlotId, XdpTableId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum XdpArch {
    Rv32Imc,
}

/// Program's current unique copy location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpLocation {
    /// Cold table entry index (not a byte offset).
    Table { index: XdpTableId },
    /// Executable slot index.
    Slot { index: XdpSlotId },
}

/// User-visible XDP program metadata.
#[derive(Clone)]
pub struct XdpMeta {
    pub name: String,
    pub link_addr: usize,
    pub image_len: usize,
    pub abi_version: u32,
    pub required_maps: Vec<String>,
    pub location: XdpLocation,
    pub(crate) arch: XdpArch,
    pub(crate) helper_address: usize,
    #[allow(dead_code)]
    pub(crate) image_digest: [u8; 32],
}

impl XdpMeta {
    pub(crate) fn set_location(&mut self, location: XdpLocation) {
        self.location = location;
    }

    pub(crate) fn location(&self) -> XdpLocation {
        self.location
    }

    pub(crate) fn check_abi(&self) -> Result<(), XdpError> {
        if self.abi_version != 1 {
            return Err(XdpError::AbiVersion);
        }
        Ok(())
    }

    pub(crate) fn check_arch(&self) -> Result<(), XdpError> {
        match self.arch {
            XdpArch::Rv32Imc => {
                #[cfg(all(target_arch = "riscv32", not(target_feature = "c")))]
                {
                    return Err(XdpError::Arch);
                }
                Ok(())
            }
        }
    }

    pub(crate) fn check_helper_address(&self) -> Result<(), XdpError> {
        if self.helper_address == 0 {
            return Err(XdpError::HelperAddress);
        }
        let expected = crate::helper::helper_dispatcher as *const () as usize;
        if expected != 0 && self.helper_address != expected {
            return Err(XdpError::HelperAddressMismatch);
        }
        Ok(())
    }

    pub(crate) fn check_slot_address(&self, slot: &XdpSlot) -> Result<(), XdpError> {
        if self.link_addr != 0 && slot.base() != self.link_addr {
            return Err(XdpError::SlotAddress);
        }
        if self.link_addr != 0 && (self.link_addr & 3) != 0 {
            return Err(XdpError::SlotAddressAlign);
        }
        Ok(())
    }

    pub(crate) fn check_required_maps(
        &self,
        maps: Option<&MapRtGuard<'_>>,
    ) -> Result<(), XdpError> {
        if self.required_maps.is_empty() {
            return Ok(());
        }
        let Some(rt) = maps else {
            return Err(XdpError::MapsRequired);
        };
        for name in &self.required_maps {
            rt.find(name).map_err(|_| XdpError::MissingMap)?;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn check_digest(&self, expected: Option<&[u8; 32]>) -> Result<(), XdpError> {
        let Some(exp) = expected else {
            return Ok(());
        };
        if self.image_digest == [0; 32] {
            return Ok(());
        }
        if &self.image_digest != exp {
            return Err(XdpError::ImageDigest);
        }
        Ok(())
    }
}
