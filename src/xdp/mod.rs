//! XDP execution runtime. Control-plane mutation is serialized by `XDP_RT`;
//! RX sees only the atomically published entry in `state`.

mod error;
pub(crate) mod image;
pub(crate) mod ingress;
mod lock;
pub(crate) mod map;
/// Board layout codegen (`metadata` + `partitions.csv` → `OUT_DIR/mem_map.rs`).
pub mod mem_map;
pub(crate) mod slot;
mod state;
pub mod stats;
pub(crate) mod table;
// TODO: `pub(crate) mod faults` — sibling error counters for ELF collect / ingress / table
// (see plan XDP ELF faults-module; not implemented this phase).

pub use error::XdpError;
pub(crate) use image::XdpMeta;
pub use image::{Complete, Incomplete, XdpBuilder, XdpLocation, XdpMaps};
pub use lock::{RtMutex, RtMutexGuard, XdpRtGuard};
pub use slot::{Slot, XdpSlot};
pub use state::XDP_RT;
pub use stats::{ERRORS, ErrorCounters, ErrorCountersSnapshot};
pub use table::storage::ColdRegion;
pub(crate) use table::storage::{DEFAULT_IMAGE_CAPACITY, EspFlashXdpTable, XdpTableStorage};

use alloc::{string::String, vec::Vec};
use core::sync::atomic::Ordering;

use crate::{
    map::{Error as MapError, MapBuilder, MapRtGuard, MapType},
    xdp::{
        map::VmrTable,
        state::{ACTIVE, IN_FLIGHT, MAX_IMAGES, MAX_MAP_REFS, MAX_SLOTS, PRIMARY_ENTRY},
    },
};
pub type XdpId = usize;
pub type SlotId = usize;

/// Boot inventory from [`XdpRt::init`]: image and map names/types only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReportItem {
    Xdp(String),
    Map(MapType, String),
}

pub(crate) struct SlotState {
    pub(crate) slot: XdpSlot,
    pub(crate) loaded_len: usize,
    pub(crate) image: Option<XdpId>,
}

pub(crate) struct ImageRecord {
    pub(crate) meta: XdpMeta,
    pub(crate) table_id: XdpId,
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
    /// Firmware-first boot: Flash `[cold_table]` + IRAM slots, scan Compact ELF,
    /// create required maps.
    ///
    /// `flash` is the chip FLASH peripheral. Packed `xdp.elf` is written with
    /// `espflash write-bin <cold.flash_base> <xdp.elf>`. Empty / erased regions
    /// register nothing. [`crate::map::MapRt`] must already be initialized (heap claimed).
    pub async fn init(
        flash: esp_hal::peripherals::FLASH<'static>,
        slots: &[XdpSlot],
        cold: ColdRegion,
        maps: &mut MapRtGuard<'_>,
    ) -> Result<(XdpRtGuard<'static>, Vec<ReportItem>), XdpError> {
        Self::assert_board_links()?;
        let table = EspFlashXdpTable::try_new(
            esp_storage::FlashStorage::new(flash),
            cold.flash_base,
            cold.flash_len,
            cold.image_capacity,
        )?;
        let storage: &'static mut dyn XdpTableStorage = unsafe {
            static mut TABLE: Option<EspFlashXdpTable> = None;
            let ptr = core::ptr::addr_of_mut!(TABLE);
            *ptr = Some(table);
            (*ptr).as_mut().unwrap()
        };
        let mut rt = Self {
            table: storage,
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
        Self::start(rt, maps).await
    }

    async fn start(
        mut rt: Self,
        maps: &mut MapRtGuard<'_>,
    ) -> Result<(XdpRtGuard<'static>, Vec<ReportItem>), XdpError> {
        if rt.primary == usize::MAX {
            return Err(XdpError::NoPrimarySlot);
        }
        let _ = rt.discover_elfs();
        let report = rt.create_required_maps(maps)?;
        if XDP_RT.init(RtMutex::new(rt)).is_err() {
            return Err(XdpError::AlreadyInitialized);
        }
        Ok((XDP_RT.get().await.lock().await, report))
    }

    /// Absolute flash address of cold-table cell `table_id` from layout codegen.
    ///
    /// Returns `None` when intent has no `[cold_table]` or `table_id` is OOB.
    pub fn cold_table_image_abs(table_id: XdpId) -> Option<usize> {
        crate::xdp::mem_map::cold_table_image_abs(table_id)
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    /// Scan the cold region for Compact ELF containers and register each image.
    ///
    /// Returns how many images were registered. [`XdpMeta`] stays crate-internal.
    /// Default [`Self::init`] already runs this once.
    pub(crate) fn discover_elfs(&mut self) -> usize {
        let metas = image::elf::collect_metas(self.table);
        let mut n = 0usize;
        for meta in metas {
            let table_id = match meta.location() {
                XdpLocation::Table { index } => index,
                XdpLocation::Slot { .. } => continue,
            };
            if self.register_meta(meta, table_id).is_ok() {
                n += 1;
            }
        }
        n
    }

    fn create_required_maps(
        &self,
        maps: &mut MapRtGuard<'_>,
    ) -> Result<Vec<ReportItem>, XdpError> {
        let mut report = Vec::new();
        for img in &self.images {
            report.push(ReportItem::Xdp(img.meta.name.clone()));
        }
        for img in &self.images {
            for spec in &img.meta.required_maps {
                if report
                    .iter()
                    .any(|item| matches!(item, ReportItem::Map(_, n) if n == &spec.name))
                {
                    maps.check(spec).map_err(map_error)?;
                    continue;
                }
                match maps.find(&spec.name) {
                    Ok(_) => {
                        maps.check(spec).map_err(map_error)?;
                    }
                    Err(MapError::NotFound) => {
                        maps.create(MapBuilder::from_meta(spec))
                            .map_err(map_error)?;
                    }
                    Err(e) => return Err(map_error(e)),
                }
                report.push(ReportItem::Map(spec.map_type, spec.name.clone()));
            }
        }
        Ok(report)
    }

    /// Append another executable slot.
    pub(crate) fn push_slot<const N: usize>(
        &mut self,
        slot: &'static Slot<N>,
        is_prim: bool,
    ) -> Result<SlotId, XdpError> {
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
    ///
    /// Map deps: use [`XdpBuilder`] with [`XdpBuilder::with_maps`] instead of this helper.
    pub fn push(&mut self, name: impl Into<String>, image: &[u8]) -> Result<XdpId, XdpError> {
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
        // Bytes now sit in the cold table; registration is the same as the Flash path.
        XdpBuilder::from_existing(table_id, image.len(), Some(name))
            .complete()?
            .build(self)
    }

    /// Register metadata for bytes already stored at `table_id`.
    ///
    /// Hand path: Flash already programmed via `espflash write-bin` at
    /// [`Self::cold_table_image_abs`]; prefer [`XdpBuilder::from_existing`] then
    /// [`Self::r#move`].
    pub(crate) fn register_meta(
        &mut self,
        meta: XdpMeta,
        table_id: XdpId,
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
            .push(ImageRecord { meta, table_id })
            .map_err(|_| XdpError::TooManyImages)?;
        Ok(id)
    }

    /// Write `image` into cold-table cell `table_id` (erase + write). Does not register meta.
    pub fn write_table_image(&mut self, table_id: XdpId, image: &[u8]) -> Result<(), XdpError> {
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
        Ok(())
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
        let (offset, erase_len) = if image.meta.container_len != 0 {
            (image.meta.container_abs, image.meta.container_len)
        } else {
            (
                table_id * self.table.image_capacity(),
                self.table.image_capacity(),
            )
        };
        self.table.erase(offset, erase_len)?;
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

    /// Check whether `id` can execute in `slot` (no map checks).
    pub fn check(&self, id: XdpId, slot: SlotId) -> Result<(), XdpError> {
        let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
        let slot_state = self.slots.get(slot).ok_or(XdpError::UnknownSlot)?;
        image.meta.check_arch()?;
        image.meta.check_helper_address()?;
        image.meta.check_slot_address(&slot_state.slot)?;
        if image.meta.image_len > slot_state.slot.len() {
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

    fn move_to_slot(&mut self, id: XdpId, slot: SlotId) -> Result<(), XdpError> {
        self.check(id, slot)?;
        let (aot_abs, image_len, link_addr) = {
            let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
            (
                image.meta.aot_abs(),
                image.meta.image_len,
                image.meta.link_addr,
            )
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
        self.table.read(aot_abs, &mut buf[..image_len])?;
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
        Ok(())
    }

    /// Copy the unique image from its current slot back into cold-table PROGBITS.
    fn move_to_table(&mut self, id: XdpId, table_id: XdpId) -> Result<(), XdpError> {
        let (image_len, from_slot, prev_table, aot_abs) = {
            let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
            match image.meta.location() {
                XdpLocation::Table { index } if index == table_id => {
                    return Ok(());
                }
                XdpLocation::Table { .. } => {
                    // Already cold but different cell: reject (no multi-copy).
                    return Err(XdpError::NotImplemented);
                }
                XdpLocation::Slot { index } => (
                    image.meta.image_len,
                    index,
                    image.table_id,
                    image.meta.aot_abs(),
                ),
            }
        };
        if table_id != prev_table {
            // Keep table_id stable with registration; only accept the recorded cell.
            return Err(XdpError::TableIdOverflow);
        }
        if image_len > self.table.image_capacity() {
            return Err(XdpError::ImageTooLarge);
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
        self.table.write(aot_abs, &buf[..image_len])?;
        let slot_state = self.slots.get_mut(from_slot).ok_or(XdpError::UnknownSlot)?;
        slot_state.loaded_len = 0;
        slot_state.image = None;
        self.images
            .get_mut(id)
            .ok_or(XdpError::UnknownImage)?
            .meta
            .set_location(XdpLocation::Table { index: table_id });
        Ok(())
    }

    pub fn set_primary(&mut self, slot: SlotId) -> Result<(), XdpError> {
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

    /// Structural check that storage matches `meta`.
    pub(crate) fn validate_image_storage(&self, meta: &XdpMeta) -> Result<(), XdpError> {
        if meta.image_len == 0 {
            return Err(XdpError::ImageEmpty);
        }
        match meta.location() {
            XdpLocation::Table { .. } => {
                let end = meta
                    .aot_abs()
                    .checked_add(meta.image_len)
                    .ok_or(XdpError::TableFull)?;
                if end > self.table.capacity() {
                    return Err(XdpError::TableFull);
                }
                if meta.image_len > self.table.image_capacity() {
                    return Err(XdpError::ImageTooLarge);
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

    pub(crate) fn pre_lease(&self, maps: Option<&MapRtGuard<'_>>) -> Result<(), XdpError> {
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
        let meta = &self.images.get(id).ok_or(XdpError::UnknownImage)?.meta;
        self.validate_image_storage(meta)?;
        self.validate_meta_for_primary_slot(meta, id)?;
        self.check_required_maps_rich(meta, maps)?;
        Ok(())
    }

    /// Slot/ABI checks for `meta` against the current primary (before map deps).
    fn validate_meta_for_primary_slot(&self, meta: &XdpMeta, id: XdpId) -> Result<(), XdpError> {
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
            match rt.check(req) {
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
    pub(crate) fn lease_mut(&mut self, maps: Option<&mut MapRtGuard<'_>>) -> Result<(), XdpError> {
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

    pub fn slot_for(&self, address: usize) -> Result<SlotId, XdpError> {
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

fn map_error(e: MapError) -> XdpError {
    match e {
        MapError::TypeMismatch => XdpError::TypeMismatch,
        MapError::AlreadyExists => XdpError::DuplicateName,
        MapError::InvalidName => XdpError::InvalidName,
        MapError::NotFound => XdpError::MissingMap,
        MapError::Oom => XdpError::MapLease,
        _ => XdpError::MapLease,
    }
}
