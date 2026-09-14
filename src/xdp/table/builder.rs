//! Unique XDP image metadata registration factory.

use alloc::{string::String, vec::Vec};

use crate::xdp::{
    XdpId, XdpRt, XdpTableId,
    error::XdpError,
    meta::{RequiredMap, XdpArch, XdpLocation, XdpMeta},
};

/// Incomplete builder marker.
#[derive(Clone, Copy, Debug, Default)]
pub struct Incomplete;

/// Ready to register into [`XdpRt`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Complete;

/// Factory for XDP metadata. The unique parse/register path for firmware images.
///
/// **Hand / Flash path (inplace):** bytes already live in the cold table (e.g.
/// programmed with `espflash write-bin` at
/// [`crate::xdp::XdpRt::cold_table_image_abs`]). Typical sequence:
///
/// ```ignore
/// let id = XdpBuilder::for_existing(0, image_len)
///     .with_name("primary")
///     .with_primary_link_addr()
///     .with_required_maps(maps)
///     .complete()?
///     .build(&mut xdp)?;
/// xdp.r#move(id, XdpLocation::Slot { index: primary })?;
/// xdp.lease(Some(&mut maps))?;
/// ```
///
/// Or the thin wrapper [`crate::xdp::XdpRt::register_inplace`].
///
/// **Push path:** [`crate::xdp::XdpRt::push`] / [`crate::xdp::XdpRt::push_with`]
/// write bytes into the cold table before [`Self::build`].
pub struct XdpBuilder<S = Incomplete> {
    name: Option<String>,
    link_addr: usize,
    abi_version: u32,
    helper_address: usize,
    required_maps: Vec<RequiredMap>,
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

    /// Start a register-inplace builder for cold-table cell `table_index`.
    ///
    /// Does not touch storage; Flash/SRAM must already hold `image_len` bytes at
    /// that cell (see [`crate::xdp::XdpRt::cold_table_image_abs`]).
    pub fn for_existing(table_index: XdpTableId, image_len: usize) -> Self {
        Self::new()
            .with_table_index(table_index)
            .with_image_len(image_len)
    }

    /// Default-path entity: firmware image that may already exist on the XdpTable.
    ///
    /// Typical sequence: `complete()?.build(rt)?` → `rt.move(…, Slot{primary})` → `lease`.
    pub fn for_firmware_image(table_index: XdpTableId, image_len: usize) -> Self {
        Self::for_existing(table_index, image_len)
            .with_name("primary")
            .with_primary_link_addr()
            .with_layout_helper_address()
    }

    /// Boot path B: push each `BOOT_XDPS` image into the table and move to its slot.
    pub fn from_bpf_toml(
        descs: &[crate::xdp::BootXdpDesc],
        rt: &mut XdpRt,
    ) -> Result<(), XdpError> {
        if descs.len() > 8 {
            return Err(XdpError::TooManyImages);
        }
        let helper_address = crate::helper::helper_dispatcher as *const () as usize;
        for image in descs {
            let helper = if image.helper_address != 0 {
                image.helper_address
            } else {
                helper_address
            };
            let required: alloc::vec::Vec<_> = image
                .required_maps
                .iter()
                .map(|m| m.to_required())
                .collect();
            let id = rt.push_at_table(
                image.name,
                image.bytes,
                image.table_id,
                image.link_addr,
                helper,
                image.abi_version,
                &required,
            )?;
            let slot = rt.slot_for(image.link_addr)?;
            rt.r#move(id, XdpLocation::Slot { index: slot })?;
        }
        Ok(())
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_link_addr(mut self, addr: usize) -> Self {
        self.link_addr = addr;
        self
    }

    /// Set `link_addr` to the board primary (`rx`) hot-slot base from layout codegen.
    pub fn with_primary_link_addr(mut self) -> Self {
        let idx = crate::xdp::mem_map::MOUNT_RX_SLOT.unwrap_or(0);
        self.link_addr = crate::xdp::mem_map::HOT_SLOT_BASES[idx];
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

    /// Set helper address from layout codegen (`HELPER_DISPATCHER_BASE`) when non-zero.
    pub fn with_layout_helper_address(mut self) -> Self {
        let base = crate::xdp::mem_map::HELPER_DISPATCHER_BASE;
        if base != 0 {
            self.helper_address = base;
        }
        self
    }

    pub fn with_required_maps<I>(mut self, maps: I) -> Self
    where
        I: IntoIterator<Item = RequiredMap>,
    {
        self.required_maps = maps.into_iter().collect();
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
        if self.helper_address == 0 {
            return Err(XdpError::HelperAddress);
        }
        let expected = crate::xdp::mem_map::HELPER_DISPATCHER_BASE;
        if expected != 0 && self.helper_address != expected {
            return Err(XdpError::HelperAddressMismatch);
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
    ///
    /// Does not read or write storage; the cold-table cell at `table_index` must
    /// already contain `image_len` valid bytes (Flash write-bin or prior `push`).
    pub fn build(self, rt: &mut XdpRt) -> Result<XdpId, XdpError> {
        let table_id = self.table_index.ok_or(XdpError::TableIdOverflow)?;
        let name = self.name.ok_or(XdpError::InvalidName)?;
        if rt.find_name(&name).is_some() {
            return Err(XdpError::DuplicateName);
        }
        let cap = rt.table.image_capacity();
        if self.image_len == 0 || self.image_len > cap {
            return Err(XdpError::ImageTooLarge);
        }
        let offset = table_id.checked_mul(cap).ok_or(XdpError::TableIdOverflow)?;
        if offset.saturating_add(cap) > rt.table.capacity() {
            return Err(XdpError::TableFull);
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
