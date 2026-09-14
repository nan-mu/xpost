//! XDP image metadata and runtime checks.

use alloc::{string::String, vec::Vec};

use crate::{
    map::{
        builder::{CompleteBuilder, MapType},
        runtime::MapRt,
    },
    xdp::{XdpSlotId, XdpTableId, error::XdpError, slot::XdpSlot},
};

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

/// Rich map dependency row carried on [`XdpMeta`].
#[derive(Clone, Debug)]
pub struct RequiredMap {
    pub name: String,
    pub map_type: MapType,
    pub key_layout: Option<Vec<u8>>,
    pub value_layout: Option<Vec<u8>>,
    /// Single-element byte size (HASH=key+value, ARRAY=value, RINGBUF=len).
    pub elem_size: usize,
    /// Talc heap budget (aligned with [`MapRt::estimate_map_heap_size`]).
    pub heap_size: usize,
    /// `max_entries` / ringbuf byte capacity ([`CompleteBuilder::len`]).
    pub len: usize,
}

impl RequiredMap {
    /// Build from layouts; `heap_size` / `elem_size` use the same helpers as map create.
    pub fn from_layouts(
        name: String,
        map_type: MapType,
        key_layout: Option<Vec<u8>>,
        value_layout: Option<Vec<u8>>,
        len: usize,
    ) -> Self {
        let heap_size = MapRt::estimate_map_heap_size(
            map_type,
            key_layout.as_deref(),
            value_layout.as_deref(),
            len,
        );
        let elem_size = MapRt::map_elem_size(
            map_type,
            key_layout.as_deref(),
            value_layout.as_deref(),
            len,
        );
        Self {
            name,
            map_type,
            key_layout,
            value_layout,
            elem_size,
            heap_size,
            len,
        }
    }

    /// Convert to a [`CompleteBuilder`] for [`MapRt::check`].
    pub fn to_complete(&self) -> CompleteBuilder {
        CompleteBuilder {
            name: self.name.clone(),
            map_type: self.map_type,
            key_layout: self.key_layout.clone(),
            value_layout: self.value_layout.clone(),
            len: self.len,
        }
    }
}

/// Static boot descriptor (codegen); converted to [`RequiredMap`] at registration.
#[derive(Clone, Copy, Debug)]
pub struct BootRequiredMap {
    pub name: &'static str,
    pub map_type: MapType,
    pub key_layout: Option<&'static [u8]>,
    pub value_layout: Option<&'static [u8]>,
    pub len: usize,
}

impl BootRequiredMap {
    pub(crate) fn to_required(&self) -> RequiredMap {
        RequiredMap::from_layouts(
            self.name.into(),
            self.map_type,
            self.key_layout.map(|l| l.to_vec()),
            self.value_layout.map(|l| l.to_vec()),
            self.len,
        )
    }
}

/// User-visible XDP program metadata.
#[derive(Clone)]
pub struct XdpMeta {
    pub name: String,
    pub link_addr: usize,
    pub image_len: usize,
    pub abi_version: u32,
    pub required_maps: Vec<RequiredMap>,
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
                if cfg!(target_feature = "c") {
                    Ok(())
                } else {
                    Err(XdpError::Arch)
                }
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
