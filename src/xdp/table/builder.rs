//! Unique XDP image metadata registration factory.

use alloc::string::String;
use alloc::vec::Vec;

use crate::xdp::error::XdpError;
use crate::xdp::meta::{XdpArch, XdpLocation, XdpMeta};
use crate::xdp::{XdpId, XdpRt, XdpTableId};

/// Incomplete builder marker.
#[derive(Clone, Copy, Debug, Default)]
pub struct Incomplete;

/// Ready to register into [`XdpRt`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Complete;

/// Factory for XDP metadata. The unique parse/register path for firmware images.
///
/// Assumes cold-table bytes already exist at [`Self::table_index`] (or will be
/// written by [`XdpRt::push`] before [`Self::build`]).
pub struct XdpBuilder<S = Incomplete> {
    name: Option<String>,
    link_addr: usize,
    abi_version: u32,
    helper_address: usize,
    required_maps: Vec<String>,
    image_digest: [u8; 32],
    table_index: Option<XdpTableId>,
    image_len: usize,
    _state: core::marker::PhantomData<S>,
}

impl XdpBuilder<Incomplete> {
    pub fn new() -> Self {
        Self {
            name: None,
            link_addr: 0,
            abi_version: 1,
            helper_address: crate::helper::helper_dispatcher as *const () as usize,
            required_maps: Vec::new(),
            image_digest: [0; 32],
            table_index: None,
            image_len: 0,
            _state: core::marker::PhantomData,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_link_addr(mut self, addr: usize) -> Self {
        self.link_addr = addr;
        self
    }

    pub fn with_abi_version(mut self, v: u32) -> Self {
        self.abi_version = v;
        self
    }

    pub fn with_helper_address(mut self, addr: usize) -> Self {
        self.helper_address = addr;
        self
    }

    pub fn with_required_maps<I, S>(mut self, maps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_maps = maps.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_image_digest(mut self, digest: [u8; 32]) -> Self {
        self.image_digest = digest;
        self
    }

    /// Cold-table slot that already holds the image bytes.
    pub fn with_table_index(mut self, index: XdpTableId) -> Self {
        self.table_index = Some(index);
        self
    }

    pub fn with_image_len(mut self, len: usize) -> Self {
        self.image_len = len;
        self
    }

    pub fn complete(self) -> Result<XdpBuilder<Complete>, XdpError> {
        if self.name.as_ref().map(|n| n.is_empty()).unwrap_or(true) {
            return Err(XdpError::InvalidName);
        }
        if self.table_index.is_none() {
            return Err(XdpError::TableIdOverflow);
        }
        if self.image_len == 0 {
            return Err(XdpError::ImageEmpty);
        }
        Ok(XdpBuilder {
            name: self.name,
            link_addr: self.link_addr,
            abi_version: self.abi_version,
            helper_address: self.helper_address,
            required_maps: self.required_maps,
            image_digest: self.image_digest,
            table_index: self.table_index,
            image_len: self.image_len,
            _state: core::marker::PhantomData,
        })
    }
}

impl Default for XdpBuilder<Incomplete> {
    fn default() -> Self {
        Self::new()
    }
}

impl XdpBuilder<Complete> {
    /// Register metadata for an image already present in the cold table.
    pub fn build(self, rt: &mut XdpRt) -> Result<XdpId, XdpError> {
        let table_id = self.table_index.ok_or(XdpError::TableIdOverflow)?;
        let name = self.name.ok_or(XdpError::InvalidName)?;
        if rt.find_name(&name).is_some() {
            return Err(XdpError::DuplicateName);
        }
        let meta = XdpMeta {
            name,
            link_addr: self.link_addr,
            image_len: self.image_len,
            abi_version: self.abi_version,
            required_maps: self.required_maps,
            location: XdpLocation::Table { index: table_id },
            arch: XdpArch::Rv32Imc,
            helper_address: self.helper_address,
            image_digest: self.image_digest,
        };
        rt.register_meta(meta, table_id, self.image_len)
    }
}
