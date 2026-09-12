//! XDP execution runtime. Control-plane mutation is serialized by `XDP_RT`;
//! RX sees only the atomically published entry in `state`.

#[cfg(feature = "debug")]
macro_rules! xdp_debug {
    ($($arg:tt)*) => {
        log::debug!($($arg)*);
    };
}
#[cfg(not(feature = "debug"))]
macro_rules! xdp_debug {
    ($($arg:tt)*) => {};
}

mod error;
pub(crate) mod ingress;
mod lock;
pub(crate) mod map;
mod meta;
/// Board layout codegen (`layout.toml` → `OUT_DIR/mem_map.rs`).
pub mod mem_map;
pub mod slot;
mod state;
pub(crate) mod switch;
pub mod table;

pub use error::XdpError;
pub use lock::{RtMutex, RtMutexGuard, XdpRtGuard};
pub use meta::{BootRequiredMap, RequiredMap, XdpLocation, XdpMeta};
pub use slot::{Slot, XdpSlot};
pub use state::XDP_RT;
pub use table::builder::{Complete, Incomplete, XdpBuilder};
pub use table::storage::{
    COLD_TABLE_BYTES, DEFAULT_IMAGE_CAPACITY, MemoryXdpTable, XdpTable, XdpTableStorage,
};

#[cfg(feature = "esp32c3-storage")]
pub use table::storage::EspFlashXdpTable;

/// Board Wi-Fi bring-up with XDP RX wrap (feature `esp32c3-radio`).
#[cfg(feature = "esp32c3-radio")]
pub use ingress::esp_radio::new;

/// Static bpf.toml XDP boot descriptor (codegen → firmware `BOOT_XDPS`).
#[derive(Clone, Copy, Debug)]
pub struct BootXdpDesc {
    pub name: &'static str,
    pub table_id: XdpTableId,
    pub link_addr: usize,
    pub helper_address: usize,
    pub abi_version: u32,
    pub required_maps: &'static [BootRequiredMap],
    pub bytes: &'static [u8],
}

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::map::MapRtGuard;
use map::VmrTable;
use meta::XdpMeta as Meta;
use state::{ACTIVE, IN_FLIGHT, MAX_IMAGES, MAX_MAP_REFS, MAX_SLOTS, PRIMARY_ENTRY};
pub type XdpId = usize;
pub type XdpSlotId = usize;
pub type XdpTableId = usize;

pub(crate) struct SlotState {
    pub(crate) slot: XdpSlot,
    pub(crate) loaded_len: usize,
    pub(crate) image: Option<XdpId>,
}

pub(crate) struct ImageRecord {
    pub(crate) meta: Meta,
    pub(crate) table_id: XdpTableId,
    pub(crate) image_len: usize,
}

pub struct XdpRt {
    pub(crate) table: &'static mut dyn XdpTableStorage,
    pub(crate) slots: heapless::Vec<SlotState, MAX_SLOTS>,
    pub(crate) images: heapless::Vec<ImageRecord, MAX_IMAGES>,
    pub(crate) primary: usize,
    pub(crate) leased: bool,
    map_refs: heapless::Vec<crate::map::MapIdx, MAX_MAP_REFS>,
    /// VMR (AOT `.maps` symbol) → RMA bindings owned by this runtime while leased.
    vmr_binds: VmrTable,
}
impl XdpRt {
    /// Register slots and an embedder-owned [`XdpTable`]; return the lock guard.
    ///
    /// Preferred entry: firmware `declare_runtime!` provides `XDP_TABLE` + `XDP_SLOTS`.
    pub async fn init<const N: usize>(
        table: &'static mut XdpTable<N>,
        slots: &'static [XdpSlot],
    ) -> Result<XdpRtGuard<'static>, XdpError> {
        let image_capacity = table.image_capacity();
        let bytes = table.as_bytes_mut();
        let storage: &'static mut dyn XdpTableStorage = unsafe {
            static mut TABLE: Option<MemoryXdpTable> = None;
            let ptr = core::ptr::addr_of_mut!(TABLE);
            *ptr = Some(MemoryXdpTable::try_with_image_capacity(bytes, image_capacity)?);
            (*ptr).as_mut().unwrap()
        };
        Self::init_with_table(slots, storage).await
    }

    /// Multi-slot init with internal SRAM cold table (legacy / tests).
    pub async fn init_slots(slots: &'static [XdpSlot]) -> Result<XdpRtGuard<'static>, XdpError> {
        Self::init_with_slots(slots).await
    }

    /// Init with an embedder-owned cold table (Flash or custom SRAM).
    pub async fn init_with_table(
        slots: &'static [XdpSlot],
        table: &'static mut dyn XdpTableStorage,
    ) -> Result<XdpRtGuard<'static>, XdpError> {
        Self::assert_board_links()?;
        let mut rt = Self {
            table,
            slots: heapless::Vec::new(),
            images: heapless::Vec::new(),
            primary: usize::MAX,
            leased: false,
            map_refs: heapless::Vec::new(),
            vmr_binds: VmrTable::new(),
        };
        for slot in slots {
            rt.register_slot(*slot)?;
        }
        if rt.primary == usize::MAX {
            return Err(XdpError::NoPrimarySlot);
        }
        if XDP_RT.init(RtMutex::new(rt)).is_err() {
            return Err(XdpError::AlreadyInitialized);
        }
        Ok(XDP_RT.get().await.lock().await)
    }

    async fn init_with_slots(slots: &[XdpSlot]) -> Result<XdpRtGuard<'static>, XdpError> {
        Self::assert_board_links()?;
        let table: &'static mut dyn XdpTableStorage = unsafe {
            static mut TABLE: Option<MemoryXdpTable> = None;
            let ptr = core::ptr::addr_of_mut!(TABLE);
            *ptr = Some(MemoryXdpTable::try_new(&mut *core::ptr::addr_of_mut!(
                table::storage::COLD_TABLE
            ))?);
            (*ptr).as_mut().unwrap()
        };
        let mut rt = Self {
            table,
            slots: heapless::Vec::new(),
            images: heapless::Vec::new(),
            primary: usize::MAX,
            leased: false,
            map_refs: heapless::Vec::new(),
            vmr_binds: VmrTable::new(),
        };
        for slot in slots {
            rt.register_slot(*slot)?;
        }
        if rt.primary == usize::MAX {
            return Err(XdpError::NoPrimarySlot);
        }
        if XDP_RT.init(RtMutex::new(rt)).is_err() {
            return Err(XdpError::AlreadyInitialized);
        }
        Ok(XDP_RT.get().await.lock().await)
    }

    /// Replace the cold table with board Flash/SRAM storage.
    ///
    /// Must be called before any image is registered and while not leased.
    /// Prefer [`Self::init_with_table`] when the backend is known at boot.
    pub fn claim_table(&mut self, table: &'static mut dyn XdpTableStorage) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        if !self.images.is_empty() {
            return Err(XdpError::Storage);
        }
        self.table = table;
        Ok(())
    }

    /// Absolute flash address of cold-table cell `table_id` from layout codegen.
    ///
    /// Returns `None` when intent has no `[cold_table]` or `table_id` is OOB.
    pub fn cold_table_image_abs(table_id: XdpTableId) -> Option<usize> {
        crate::xdp::mem_map::cold_table_image_abs(table_id)
    }

    /// Append another executable slot.
    pub fn push_slot<const N: usize>(
        &mut self,
        slot: &'static Slot<N>,
        is_prim: bool,
    ) -> Result<XdpSlotId, XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        let desc = unsafe { slot.as_xdp_slot(is_prim) };
        let id = self.slots.len();
        self.register_slot(desc)?;
        Ok(id)
    }

    fn assert_board_links() -> Result<(), XdpError> {
        let helper = crate::helper::helper_dispatcher as *const () as usize;
        if crate::xdp::mem_map::HELPER_DISPATCHER_BASE == 0 {
            return Ok(());
        }
        if helper != crate::xdp::mem_map::HELPER_DISPATCHER_BASE {
            return Err(XdpError::LinkedAddress);
        }
        Ok(())
    }

    fn register_slot(&mut self, slot: XdpSlot) -> Result<(), XdpError> {
        slot.validate()?;
        let end = slot
            .base()
            .checked_add(slot.len())
            .ok_or(XdpError::SlotOverflow)?;
        if self.slots.iter().any(|old| {
            slot.base() < old.slot.base().saturating_add(old.slot.len()) && old.slot.base() < end
        }) {
            return Err(XdpError::OverlappingSlots);
        }
        if slot.is_primary() {
            if self.primary != usize::MAX {
                return Err(XdpError::MultiplePrimarySlots);
            }
            self.primary = self.slots.len();
        }
        self.slots
            .push(SlotState {
                slot,
                loaded_len: 0,
                image: None,
            })
            .map_err(|_| XdpError::TooManySlots)?;
        Ok(())
    }

    pub(crate) fn find_name(&self, name: &str) -> Option<XdpId> {
        self.images.iter().position(|img| img.meta.name == name)
    }

    /// Write image bytes into the cold table and register metadata via [`XdpBuilder`].
    pub fn push(&mut self, name: impl Into<String>, image: &[u8]) -> Result<XdpId, XdpError> {
        self.push_with(name, image, 0, None)
    }

    /// Like [`Self::push`], with explicit `link_addr` and optional rich map deps.
    pub fn push_with(
        &mut self,
        name: impl Into<String>,
        image: &[u8],
        link_addr: usize,
        required_maps: Option<&[RequiredMap]>,
    ) -> Result<XdpId, XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        if image.is_empty() {
            return Err(XdpError::ImageEmpty);
        }
        if image.len() > self.table.image_capacity() {
            return Err(XdpError::ImageTooLarge);
        }
        let table_id = self.images.len();
        let offset = table_id
            .checked_mul(self.table.image_capacity())
            .ok_or(XdpError::TableIdOverflow)?;
        if offset.saturating_add(self.table.image_capacity()) > self.table.capacity() {
            return Err(XdpError::TableFull);
        }
        self.table.erase(offset, self.table.image_capacity())?;
        self.table.write(offset, image)?;
        let mut b = XdpBuilder::new()
            .with_name(name)
            .with_table_index(table_id)
            .with_image_len(image.len())
            .with_link_addr(link_addr);
        if let Some(maps) = required_maps {
            b = b.with_required_maps(maps.iter().cloned());
        }
        b.complete()?.build(self)
    }

    /// Register metadata for an image already present at cold-table cell `table_id`.
    ///
    /// Flash hand path: host wrote bytes via `espflash write-bin` to
    /// [`Self::cold_table_image_abs`]; this only creates [`XdpMeta`]. Follow with
    /// [`Self::r#move`] into the primary slot and [`Self::lease`].
    pub fn register_inplace(
        &mut self,
        name: impl Into<String>,
        table_id: XdpTableId,
        image_len: usize,
        required_maps: Option<&[RequiredMap]>,
    ) -> Result<XdpId, XdpError> {
        let mut b = XdpBuilder::for_existing(table_id, image_len)
            .with_name(name)
            .with_primary_link_addr()
            .with_layout_helper_address();
        if let Some(maps) = required_maps {
            b = b.with_required_maps(maps.iter().cloned());
        }
        b.complete()?.build(self)
    }

    /// Register metadata for bytes already stored at `table_id`.
    ///
    /// Hand path: Flash already programmed via `espflash write-bin` at
    /// [`Self::cold_table_image_abs`]; prefer [`Self::register_inplace`] or
    /// [`XdpBuilder`] then [`Self::r#move`].
    pub(crate) fn register_meta(
        &mut self,
        meta: Meta,
        table_id: XdpTableId,
        image_len: usize,
    ) -> Result<XdpId, XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        if self.images.len() > table_id && self.images[table_id].table_id == table_id {
            // Replacing meta for an existing table cell is not supported.
            return Err(XdpError::DuplicateName);
        }
        let id = self.images.len();
        if id != table_id {
            // Dense table ids: builder path after push uses images.len() as id.
            // Forced table_id from boot may skip — allow if table_id == id.
            if table_id != id {
                // Boot may force table_id == sequential index.
                let _ = table_id;
            }
        }
        self.images
            .push(ImageRecord {
                meta,
                table_id,
                image_len,
            })
            .map_err(|_| XdpError::TooManyImages)?;
        Ok(id)
    }

    /// Write image to a forced table cell then register via builder (boot path).
    pub(crate) fn push_at_table(
        &mut self,
        name: impl Into<String>,
        image: &[u8],
        table_id: XdpTableId,
        link_addr: usize,
        helper_address: usize,
        abi_version: u32,
        required_maps: &[RequiredMap],
    ) -> Result<XdpId, XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        if image.is_empty() {
            return Err(XdpError::ImageEmpty);
        }
        if image.len() > self.table.image_capacity() {
            return Err(XdpError::ImageTooLarge);
        }
        let offset = table_id
            .checked_mul(self.table.image_capacity())
            .ok_or(XdpError::TableIdOverflow)?;
        if offset.saturating_add(self.table.image_capacity()) > self.table.capacity() {
            return Err(XdpError::TableFull);
        }
        self.table.erase(offset, self.table.image_capacity())?;
        self.table.write(offset, image)?;
        XdpBuilder::new()
            .with_name(name)
            .with_table_index(table_id)
            .with_image_len(image.len())
            .with_link_addr(link_addr)
            .with_helper_address(helper_address)
            .with_abi_version(abi_version)
            .with_required_maps(required_maps.iter().cloned())
            .complete()?
            .build(self)
    }

    pub fn find(&self, name: &str) -> Result<XdpId, XdpError> {
        self.find_name(name).ok_or(XdpError::UnknownImage)
    }

    pub fn delete(&mut self, id: XdpId) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
        if let XdpLocation::Slot { index } = image.meta.location() {
            if index == self.primary {
                return Err(XdpError::Leased);
            }
            if let Some(slot) = self.slots.get_mut(index) {
                if slot.image == Some(id) {
                    slot.loaded_len = 0;
                    slot.image = None;
                }
            }
        }
        let table_id = image.table_id;
        let cap = self.table.image_capacity();
        let offset = table_id * cap;
        self.table.erase(offset, cap)?;
        self.images.remove(id);
        // Fix up slot image ids after dense remove.
        for slot in self.slots.iter_mut() {
            if let Some(img) = slot.image {
                if img > id {
                    slot.image = Some(img - 1);
                } else if img == id {
                    slot.image = None;
                    slot.loaded_len = 0;
                }
            }
        }
        Ok(())
    }

    pub fn meta(&self, id: XdpId) -> Result<&XdpMeta, XdpError> {
        self.images
            .get(id)
            .map(|img| &img.meta)
            .ok_or(XdpError::UnknownImage)
    }

    /// Check whether `id` can execute in `slot` (no map checks).
    pub fn check(&self, id: XdpId, slot: XdpSlotId) -> Result<(), XdpError> {
        let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
        let slot_state = self.slots.get(slot).ok_or(XdpError::UnknownSlot)?;
        image.meta.check_abi()?;
        image.meta.check_arch()?;
        image.meta.check_helper_address()?;
        image.meta.check_slot_address(&slot_state.slot)?;
        if image.image_len > slot_state.slot.len() {
            return Err(XdpError::ImageTooLarge);
        }
        Ok(())
    }

    /// Move the unique copy of `id` to `loc`.
    pub fn r#move(&mut self, id: XdpId, loc: XdpLocation) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        match loc {
            XdpLocation::Slot { index } => self.move_to_slot(id, index),
            XdpLocation::Table { index } => self.move_to_table(id, index),
        }
    }

    fn move_to_slot(&mut self, id: XdpId, slot: XdpSlotId) -> Result<(), XdpError> {
        self.check(id, slot)?;
        let (table_id, image_len, link_addr) = {
            let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
            (image.table_id, image.image_len, image.meta.link_addr)
        };
        let (dst, capacity, occupied) = {
            let slot_state = self.slots.get(slot).ok_or(XdpError::UnknownSlot)?;
            if slot_state.slot.base() != link_addr && link_addr != 0 {
                return Err(XdpError::SlotMismatch);
            }
            (
                slot_state.slot.base() as *mut u8,
                slot_state.slot.len(),
                slot_state.image,
            )
        };
        if occupied.is_some() && occupied != Some(id) {
            return Err(XdpError::SlotOccupied);
        }
        if image_len > capacity {
            return Err(XdpError::ImageTooLarge);
        }
        let mut buf = [0u8; DEFAULT_IMAGE_CAPACITY];
        if image_len > buf.len() {
            return Err(XdpError::ScratchTooSmall);
        }
        let offset = table_id * self.table.image_capacity();
        self.table.read(offset, &mut buf[..image_len])?;
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, image_len);
            core::ptr::write_bytes(dst.add(image_len), 0, capacity - image_len);
        }
        Self::fence_instruction_cache();
        let slot_state = self.slots.get_mut(slot).ok_or(XdpError::UnknownSlot)?;
        slot_state.loaded_len = image_len;
        slot_state.image = Some(id);
        self.images
            .get_mut(id)
            .ok_or(XdpError::UnknownImage)?
            .meta
            .set_location(XdpLocation::Slot { index: slot });
        xdp_debug!("[xdp][move] id={id} slot={slot}");
        Ok(())
    }

    /// Copy the unique image from its current slot back into cold-table cell `table_id`.
    fn move_to_table(&mut self, id: XdpId, table_id: XdpTableId) -> Result<(), XdpError> {
        let (image_len, from_slot, prev_table) = {
            let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
            match image.meta.location() {
                XdpLocation::Table { index } if index == table_id => {
                    return Ok(());
                }
                XdpLocation::Table { .. } => {
                    // Already cold but different cell: reject (no multi-copy).
                    return Err(XdpError::NotImplemented);
                }
                XdpLocation::Slot { index } => (image.image_len, index, image.table_id),
            }
        };
        if table_id != prev_table {
            // Keep table_id stable with registration; only accept the recorded cell.
            return Err(XdpError::TableIdOverflow);
        }
        let cap = self.table.image_capacity();
        if image_len > cap {
            return Err(XdpError::ImageTooLarge);
        }
        let offset = table_id
            .checked_mul(cap)
            .ok_or(XdpError::TableIdOverflow)?;
        if offset.saturating_add(cap) > self.table.capacity() {
            return Err(XdpError::TableFull);
        }
        let mut buf = [0u8; DEFAULT_IMAGE_CAPACITY];
        if image_len > buf.len() {
            return Err(XdpError::ScratchTooSmall);
        }
        let slot_state = self.slots.get(from_slot).ok_or(XdpError::UnknownSlot)?;
        if slot_state.image != Some(id) {
            return Err(XdpError::MetaDisagree);
        }
        let src = slot_state.slot.base() as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), image_len);
        }
        self.table.erase(offset, cap)?;
        self.table.write(offset, &buf[..image_len])?;
        let slot_state = self.slots.get_mut(from_slot).ok_or(XdpError::UnknownSlot)?;
        slot_state.loaded_len = 0;
        slot_state.image = None;
        self.images
            .get_mut(id)
            .ok_or(XdpError::UnknownImage)?
            .meta
            .set_location(XdpLocation::Table { index: table_id });
        xdp_debug!("[xdp][move] id={id} table={table_id}");
        Ok(())
    }

    pub fn set_primary(&mut self, slot: XdpSlotId) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        let slot_state = self.slots.get(slot).ok_or(XdpError::UnknownSlot)?;
        if slot_state.loaded_len == 0 {
            return Err(XdpError::PrimaryEmpty);
        }
        self.primary = slot;
        Ok(())
    }

    /// Structural / future magic check that storage matches `meta` (hook; magic TODO).
    pub fn validate_image_storage(&self, meta: &XdpMeta) -> Result<(), XdpError> {
        if meta.image_len == 0 {
            return Err(XdpError::ImageEmpty);
        }
        match meta.location() {
            XdpLocation::Table { index } => {
                let cap = self.table.image_capacity();
                if meta.image_len > cap {
                    return Err(XdpError::ImageTooLarge);
                }
                let offset = index
                    .checked_mul(cap)
                    .ok_or(XdpError::TableIdOverflow)?;
                if offset.saturating_add(cap) > self.table.capacity() {
                    return Err(XdpError::TableFull);
                }
                let _ = (XdpError::ImageMagic, XdpError::ImageMismatch);
                Ok(())
            }
            XdpLocation::Slot { index } => {
                let slot = self.slots.get(index).ok_or(XdpError::UnknownSlot)?;
                if meta.image_len > slot.slot.len() {
                    return Err(XdpError::ImageTooLarge);
                }
                let _ = (XdpError::ImageMagic, XdpError::ImageMismatch);
                Ok(())
            }
        }
    }

    pub fn pre_lease(&self, maps: Option<&MapRtGuard<'_>>) -> Result<(), XdpError> {
        self.check_primary_ready(maps)
    }

    /// Shared enablement path: primary → XdpId → meta → slot/attrs → maps.
    fn check_primary_ready(&self, maps: Option<&MapRtGuard<'_>>) -> Result<(), XdpError> {
        if self.primary == usize::MAX {
            return Err(XdpError::NoPrimarySlot);
        }
        let slot_state = self
            .slots
            .get(self.primary)
            .ok_or(XdpError::NoPrimarySlot)?;
        if slot_state.loaded_len == 0 {
            return Err(XdpError::PrimaryEmpty);
        }
        let Some(id) = slot_state.image else {
            // No assume_injected: primary must have an XdpMeta row.
            return Err(XdpError::PrimaryEmpty);
        };
        let meta = self.meta(id)?;
        self.validate_image_storage(meta)?;
        self.validate_meta_for_primary_slot(meta, id)?;
        self.check_required_maps_rich(meta, maps)?;
        Ok(())
    }

    /// Slot/ABI checks for `meta` against the current primary (before map deps).
    fn validate_meta_for_primary_slot(
        &self,
        meta: &XdpMeta,
        id: XdpId,
    ) -> Result<(), XdpError> {
        let primary = self.primary;
        // ABI/arch/helper/link_addr/image_len vs primary slot.
        self.check(id, primary)?;
        match meta.location() {
            XdpLocation::Slot { index } if index == primary => Ok(()),
            _ => Err(XdpError::SlotMismatch),
        }
    }

    fn check_required_maps_rich(
        &self,
        meta: &XdpMeta,
        maps: Option<&MapRtGuard<'_>>,
    ) -> Result<(), XdpError> {
        if meta.required_maps.is_empty() {
            return Ok(());
        }
        let Some(rt) = maps else {
            return Err(XdpError::MapsRequired);
        };
        for req in &meta.required_maps {
            let expected = crate::map::MapRt::estimate_map_heap_size(
                req.map_type,
                req.key_layout.as_deref(),
                req.value_layout.as_deref(),
                req.len,
            );
            if req.heap_size != expected {
                return Err(XdpError::TypeMismatch);
            }
            let spec = req.to_complete();
            match rt.check(&spec) {
                Ok(_) => {}
                Err(crate::map::Error::NotFound) => return Err(XdpError::MissingMap),
                Err(crate::map::Error::TypeMismatch) => return Err(XdpError::TypeMismatch),
                Err(_) => return Err(XdpError::MissingMap),
            }
        }
        Ok(())
    }

    /// Enable the datapath: publish primary entry and install VMR→RMA aliases.
    ///
    /// Requires `&mut MapRtGuard` so map refcounts and the helper alias table can
    /// be updated. Pass `None` only when the leased image has no `required_maps`.
    pub fn lease(&mut self, maps: Option<&mut MapRtGuard<'_>>) -> Result<(), XdpError> {
        self.lease_inner(maps)
    }

    /// Same as [`Self::lease`] (kept for call sites that already use this name).
    pub fn lease_mut(&mut self, maps: Option<&mut MapRtGuard<'_>>) -> Result<(), XdpError> {
        self.lease_inner(maps)
    }

    fn lease_inner(&mut self, maps: Option<&mut MapRtGuard<'_>>) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        let maps_ref = maps.as_deref();
        self.pre_lease(maps_ref)?;
        let entry = {
            let slot = self
                .slots
                .get(self.primary)
                .ok_or(XdpError::NoPrimarySlot)?;
            slot.slot.base()
        };
        self.vmr_binds.clear();
        self.map_refs.clear();
        if let Some(maps) = maps {
            for &(name, vma) in crate::map::AOT_MAP_BINDS {
                if vma == 0 {
                    continue;
                }
                let map_id = maps.find(name).map_err(|_| XdpError::RequiredMapAbsent)?;
                let rma = maps.storage_ptr(map_id).map_err(|_| XdpError::MapLease)?;
                maps.add_user(map_id).map_err(|_| XdpError::MapLease)?;
                if self.map_refs.push(map_id).is_err() {
                    self.rollback_map_users(maps);
                    return Err(XdpError::MapLease);
                }
                if self.vmr_binds.insert(vma, map_id, rma).is_err() {
                    self.rollback_map_users(maps);
                    return Err(XdpError::MapLease);
                }
            }
            // Also lease required_maps by name if not already in AOT binds.
            if let Some(id) = self.slots.get(self.primary).and_then(|s| s.image) {
                let names: Vec<String> = self.images[id]
                    .meta
                    .required_maps
                    .iter()
                    .map(|r| r.name.clone())
                    .collect();
                for name in names {
                    if maps.find(&name).is_err() {
                        self.rollback_map_users(maps);
                        return Err(XdpError::MissingMap);
                    }
                    let map_id = maps.find(&name).unwrap();
                    if !self.map_refs.iter().any(|m| *m == map_id) {
                        maps.add_user(map_id).map_err(|_| XdpError::MapLease)?;
                        if self.map_refs.push(map_id).is_err() {
                            self.rollback_map_users(maps);
                            return Err(XdpError::MapLease);
                        }
                    }
                }
            }
            // Install codegen aliases only when present. Empty AOT_MAP_BINDS must
            // not call set_map_aliases(&[]) — that would wipe hand `bind_map_symbols`.
            let binds = self.vmr_binds.alias_pairs();
            if !binds.is_empty() {
                crate::map::set_map_aliases(&binds).map_err(|_| XdpError::MapLease)?;
            }
        } else if let Some(id) = self.slots.get(self.primary).and_then(|s| s.image) {
            if !self.images[id].meta.required_maps.is_empty() {
                return Err(XdpError::MapsRequired);
            }
        }
        PRIMARY_ENTRY.store(entry, Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        self.leased = true;
        Ok(())
    }

    fn rollback_map_users(&mut self, maps: &mut MapRtGuard<'_>) {
        for id in self.map_refs.drain(..) {
            let _ = maps.remove_user(id);
        }
        self.vmr_binds.clear();
        crate::map::clear_map_aliases();
    }

    pub async fn lease_async(&mut self, maps: Option<&mut MapRtGuard<'_>>) -> Result<(), XdpError> {
        self.lease(maps)
    }

    pub fn release(&mut self) -> Result<(), XdpError> {
        if !self.leased {
            return Err(XdpError::NotLeased);
        }
        ACTIVE.store(false, Ordering::Release);
        while IN_FLIGHT.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
        PRIMARY_ENTRY.store(0, Ordering::Release);
        crate::map::clear_map_aliases();
        for id in self.map_refs.drain(..) {
            let _ = crate::map::runtime::remove_user_sync(id);
        }
        self.vmr_binds.clear();
        self.leased = false;
        Ok(())
    }

    pub(crate) fn slot_for(&self, address: usize) -> Result<XdpSlotId, XdpError> {
        self.slots
            .iter()
            .position(|slot| slot.slot.base() == address)
            .ok_or(XdpError::UnknownSlot)
    }

    pub(crate) fn fence_instruction_cache() {
        unsafe {
            core::arch::asm!("fence.i");
        }
    }
}
