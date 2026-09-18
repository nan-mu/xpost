//! Unique XDP image metadata registration factory.

use alloc::{format, string::String};

use crate::{
    map::{MapBuilder, MapId, MapMeta, MapRt, MapType},
    xdp::{SlotId, XdpId, XdpRt, error::XdpError, state::MAX_REQUIRED_MAPS},
};

use super::meta::{XdpLocation, XdpMeta};

/// Incomplete builder marker — only configuration methods.
#[derive(Clone, Copy, Debug, Default)]
pub struct Incomplete;

/// Ready to register into [`XdpRt`] — only [`XdpBuilder::build`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Complete;

/// Map dependency for [`XdpBuilder::with_maps`]: spec or live `(MapId, MapRt)`.
pub enum XdpMaps<'a> {
    /// Left: declare from a [`MapBuilder`] spec (not yet necessarily created).
    Builder(MapBuilder),
    /// Right: bind an existing map in [`MapRt`].
    Registered(MapId, &'a MapRt),
}

/// Registers metadata for an XDP image **already** present in the cold Flash region
/// (programmed before boot). Does not write image bytes.
///
/// Only runtime fields are retained (slot, helper, maps, image flash span, digest).
/// Compact ELF wire temporaries are validated in [`crate::xdp::image::elf`] and discarded.
pub struct XdpBuilder<S = Incomplete> {
    pub(crate) name: Option<String>,
    pub(crate) link_addr: usize,
    pub(crate) helper_address: usize,
    pub(crate) required_maps: heapless::Vec<MapMeta, MAX_REQUIRED_MAPS>,
    pub(crate) aot_binds: heapless::Vec<(String, usize), MAX_REQUIRED_MAPS>,
    pub(crate) image_digest: [u8; 32],
    pub(crate) table_index: XdpId,
    pub(crate) image_abs: usize,
    pub(crate) image_len: usize,
    pub(crate) container_abs: usize,
    pub(crate) container_len: usize,
    _state: core::marker::PhantomData<S>,
}

impl XdpBuilder<Incomplete> {
    /// Start a builder for cold-table cell `table_index` holding `image_len` bytes.
    ///
    /// `name` is optional; when omitted, `complete` assigns `xdp_{table_index}`.
    pub fn from_existing(
        table_index: XdpId,
        image_len: usize,
        name: Option<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.map(Into::into),
            link_addr: 0,
            helper_address: crate::helper::helper_dispatcher as *const () as usize,
            required_maps: heapless::Vec::new(),
            aot_binds: heapless::Vec::new(),
            image_digest: [0; 32],
            table_index,
            image_abs: 0,
            image_len,
            container_abs: 0,
            container_len: 0,
            _state: core::marker::PhantomData,
        }
    }

    /// Empty builder for Compact ELF scan (logical id `table_index`).
    pub(crate) fn for_elf(table_index: XdpId) -> Self {
        Self {
            name: None,
            link_addr: 0,
            helper_address: 0,
            required_maps: heapless::Vec::new(),
            aot_binds: heapless::Vec::new(),
            image_digest: [0; 32],
            table_index,
            image_abs: 0,
            image_len: 0,
            container_abs: 0,
            container_len: 0,
            _state: core::marker::PhantomData,
        }
    }

    /// Record a planned slot link address from codegen (`SLOT_BASES[slot]`).
    ///
    /// Returns [`XdpError::UnknownSlot`] when the index is OOB or the base is
    /// still 0 (firmware-owned buffer — pass the real address via the ELF path).
    pub fn with_slot(mut self, slot: SlotId) -> Result<Self, XdpError> {
        let base = crate::xdp::mem_map::SLOT_BASES
            .get(slot)
            .copied()
            .ok_or(XdpError::UnknownSlot)?;
        if base == 0 {
            return Err(XdpError::UnknownSlot);
        }
        self.link_addr = base;
        Ok(self)
    }

    /// Compatibility no-op (wire `xpost_version` is validated at ELF load, not stored).
    pub fn with_abi_version(self, _v: u32) -> Self {
        self
    }

    /// Set helper address from layout codegen (`HELPER_DISPATCHER_BASE`) when non-zero.
    pub fn with_helper_address(mut self) -> Self {
        let base = crate::xdp::mem_map::HELPER_DISPATCHER_BASE;
        if base != 0 {
            self.helper_address = base;
        }
        self
    }

    /// Attach map dependencies: [`XdpMaps::Builder`] or [`XdpMaps::Registered`].
    pub fn with_maps<'a, I>(mut self, deps: I) -> Result<Self, XdpError>
    where
        I: IntoIterator<Item = XdpMaps<'a>>,
    {
        for dep in deps {
            match dep {
                XdpMaps::Builder(builder) => {
                    let meta =
                        MapMeta::from_builder(builder).map_err(|_| XdpError::TypeMismatch)?;
                    self.required_maps
                        .push(meta)
                        .map_err(|_| XdpError::TooManyImages)?;
                }
                XdpMaps::Registered(id, maps) => {
                    let meta = maps.meta.get(id).ok_or(XdpError::RequiredMapAbsent)?;
                    self.required_maps
                        .push(meta.spec_clone())
                        .map_err(|_| XdpError::TooManyImages)?;
                }
            }
        }
        Ok(self)
    }

    pub fn with_image_digest(mut self, digest: [u8; 32]) -> Self {
        self.image_digest = digest;
        self
    }

    /// Assemble [`XdpMeta`] after ELF load filled runtime fields. `None` = fail closed.
    pub(crate) fn finish_meta(self) -> Option<XdpMeta> {
        if self.image_len == 0 || self.container_len == 0 {
            return None;
        }
        if self.helper_address == 0 || (self.helper_address & 3) != 0 {
            return None;
        }
        let expected = crate::xdp::mem_map::HELPER_DISPATCHER_BASE;
        if expected != 0 && self.helper_address != expected {
            return None;
        }
        if self.link_addr == 0 || (self.link_addr & 3) != 0 {
            return None;
        }
        let name = self.name.filter(|n| !n.is_empty())?;
        if self.image_digest == [0u8; 32] {
            return None;
        }
        Some(XdpMeta {
            name,
            link_addr: self.link_addr,
            helper_address: self.helper_address,
            required_maps: self.required_maps,
            aot_binds: self.aot_binds,
            image_digest: self.image_digest,
            image_abs: self.image_abs,
            image_len: self.image_len,
            container_abs: self.container_abs,
            container_len: self.container_len,
            location: XdpLocation::Table {
                index: self.table_index,
            },
        })
    }

    pub fn complete(self) -> Result<XdpBuilder<Complete>, XdpError> {
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
        let name = match self.name {
            Some(n) if !n.is_empty() => n,
            Some(_) => return Err(XdpError::InvalidName),
            None => format!("xdp_{}", self.table_index),
        };
        Ok(XdpBuilder {
            name: Some(name),
            link_addr: self.link_addr,
            helper_address: self.helper_address,
            required_maps: self.required_maps,
            aot_binds: self.aot_binds,
            image_digest: self.image_digest,
            table_index: self.table_index,
            image_abs: self.image_abs,
            image_len: self.image_len,
            container_abs: self.container_abs,
            container_len: self.container_len,
            _state: core::marker::PhantomData,
        })
    }
}

impl XdpBuilder<Complete> {
    /// Register metadata for an image already present in the cold table.
    pub fn build(self, rt: &mut XdpRt) -> Result<XdpId, XdpError> {
        let table_id = self.table_index;
        let name = self.name.ok_or(XdpError::InvalidName)?;
        if rt.find_name(&name).is_some() {
            return Err(XdpError::DuplicateName);
        }
        let cap = rt.table.image_capacity();
        if self.image_len == 0 || self.image_len > cap {
            return Err(XdpError::ImageTooLarge);
        }
        let (container_abs, container_len, image_abs) = if self.container_len != 0 {
            if self.container_len > cap {
                return Err(XdpError::ImageTooLarge);
            }
            if self.container_abs.saturating_add(self.container_len) > rt.table.capacity() {
                return Err(XdpError::TableFull);
            }
            (self.container_abs, self.container_len, self.image_abs)
        } else {
            let offset = table_id.checked_mul(cap).ok_or(XdpError::TableIdOverflow)?;
            if offset.saturating_add(cap) > rt.table.capacity() {
                return Err(XdpError::TableFull);
            }
            (offset, self.image_len, offset)
        };
        let meta = XdpMeta {
            name,
            link_addr: self.link_addr,
            helper_address: self.helper_address,
            required_maps: self.required_maps,
            aot_binds: self.aot_binds,
            image_digest: self.image_digest,
            image_abs,
            image_len: self.image_len,
            container_abs,
            container_len,
            location: XdpLocation::Table { index: table_id },
        };
        rt.register_meta(meta, table_id)
    }
}

/// Linux BPF map type numbers used in Compact ELF MapDescriptorV0.
pub(crate) const BPF_MAP_TYPE_HASH: u8 = 1;
pub(crate) const BPF_MAP_TYPE_ARRAY: u8 = 2;

const MAX_LAYOUT_DESCS: usize = 32;
const MAX_KEY_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 256;
const MAX_ENTRIES: u32 = 64;

fn validate_layout(layout: &[u8], max_bytes: usize) -> Result<(), XdpError> {
    if layout.is_empty() || layout.len() > MAX_LAYOUT_DESCS {
        return Err(XdpError::TypeMismatch);
    }
    let mut bits: usize = 0;
    for &b in layout {
        if b == 0 {
            return Err(XdpError::TypeMismatch);
        }
        bits = bits.checked_add(b as usize).ok_or(XdpError::TypeMismatch)?;
    }
    if bits % 8 != 0 || bits / 8 > max_bytes {
        return Err(XdpError::TypeMismatch);
    }
    Ok(())
}

/// Build a [`MapBuilder`] from a Compact ELF MapDescriptorV0 wire record.
pub(crate) fn map_builder_from_descriptor(
    name: &str,
    map_type: u8,
    len: u32,
    key_layout: Option<&[u8]>,
    value_layout: Option<&[u8]>,
) -> Result<MapBuilder, XdpError> {
    if name.is_empty() || name.len() > 31 || !name.is_ascii() {
        return Err(XdpError::InvalidName);
    }
    if !(1..=MAX_ENTRIES).contains(&len) {
        return Err(XdpError::TypeMismatch);
    }
    match map_type {
        BPF_MAP_TYPE_HASH => {
            let key = key_layout.ok_or(XdpError::TypeMismatch)?;
            let value = value_layout.ok_or(XdpError::TypeMismatch)?;
            validate_layout(key, MAX_KEY_BYTES)?;
            validate_layout(value, MAX_VALUE_BYTES)?;
            Ok(MapBuilder::hash()
                .with_name(name)
                .with_key_layout(key)
                .with_value_layout(value)
                .with_capacity(len as usize))
        }
        BPF_MAP_TYPE_ARRAY => {
            if key_layout.is_some() {
                return Err(XdpError::TypeMismatch);
            }
            let value = value_layout.ok_or(XdpError::TypeMismatch)?;
            validate_layout(value, MAX_VALUE_BYTES)?;
            Ok(MapBuilder::array()
                .with_name(name)
                .with_value_layout(value)
                .with_capacity(len as usize))
        }
        _ => Err(XdpError::TypeMismatch),
    }
}

#[allow(dead_code)]
pub(crate) fn map_type_wire(t: MapType) -> Option<u8> {
    match t {
        MapType::Hash => Some(BPF_MAP_TYPE_HASH),
        MapType::Array => Some(BPF_MAP_TYPE_ARRAY),
        #[cfg(feature = "ringbuf")]
        MapType::Ringbuf => None,
    }
}
