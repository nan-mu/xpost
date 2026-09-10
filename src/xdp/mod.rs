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
mod meta;
mod slot;
mod state;
pub(crate) mod switch;
pub(crate) mod table;

pub use error::XdpError;
pub use lock::{RtMutex, RtMutexGuard, XdpRtGuard};
pub use meta::{XdpLocation, XdpMeta};
pub use slot::{Slot, XdpSlot};
pub use state::XDP_RT;
pub use table::builder::{Complete, Incomplete, XdpBuilder};
pub(crate) use table::storage::{
    COLD_TABLE_BYTES, DEFAULT_IMAGE_CAPACITY, MemoryXdpTable, XdpTableStorage,
};

#[cfg(feature = "esp32c3-storage")]
pub(crate) use table::storage::EspFlashXdpTable;

/// Board Wi-Fi bring-up with XDP RX wrap (feature `esp32c3-radio`).
#[cfg(feature = "esp32c3-radio")]
pub use ingress::esp_radio::new;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use crate::map::MapRtGuard;
use meta::XdpMeta as Meta;
use state::{ACTIVE, IN_FLIGHT, MAX_IMAGES, MAX_MAP_REFS, MAX_SLOTS, MAX_VMR_BINDS, PRIMARY_ENTRY};
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

/// `(vma, map_id)` published for helpers during lease. RMA is `RawResource.ptr`.
pub(crate) struct VmrBind {
    pub vma: usize,
    pub map_id: crate::map::MapId,
    pub rma: usize,
}

pub struct XdpRt {
    pub(crate) table: &'static mut dyn XdpTableStorage,
    pub(crate) slots: heapless::Vec<SlotState, MAX_SLOTS>,
    pub(crate) images: heapless::Vec<ImageRecord, MAX_IMAGES>,
    pub(crate) primary: usize,
    pub(crate) leased: bool,
    map_refs: heapless::Vec<crate::map::MapIdx, MAX_MAP_REFS>,
    /// VMR (AOT `.maps` symbol) → RMA bindings owned by this runtime while leased.
    vmr_binds: heapless::Vec<VmrBind, MAX_VMR_BINDS>,
}

/// Internal cold-image table (512 KiB). Not exposed to embedders.
///
/// TODO: board-configurable capacity / flash-backed table without a public trait.
static mut COLD_TABLE: [u8; COLD_TABLE_BYTES] = [0u8; COLD_TABLE_BYTES];

impl XdpRt {
    /// Register a primary slot, create the internal cold table, return the lock guard.
    pub async fn init<const N: usize>(
        primary: &'static Slot<N>,
    ) -> Result<XdpRtGuard<'static>, XdpError> {
        let slot = unsafe { primary.as_xdp_slot(true) };
        // `XdpSlot` is `Copy`; init copies descriptors into owned storage.
        Self::init_with_slots(&[slot]).await
    }

    /// Multi-slot init used by board codegen (`firmware_xdp_slots`).
    pub async fn init_slots(slots: &'static [XdpSlot]) -> Result<XdpRtGuard<'static>, XdpError> {
        Self::init_with_slots(slots).await
    }

    async fn init_with_slots(slots: &[XdpSlot]) -> Result<XdpRtGuard<'static>, XdpError> {
        Self::assert_board_links()?;
        let table: &'static mut dyn XdpTableStorage = unsafe {
            static mut TABLE: Option<MemoryXdpTable> = None;
            let ptr = core::ptr::addr_of_mut!(TABLE);
            *ptr = Some(MemoryXdpTable::try_new(&mut *core::ptr::addr_of_mut!(
                COLD_TABLE
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
            vmr_binds: heapless::Vec::new(),
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
        if crate::mem_map::HELPER_DISPATCHER_BASE == 0 {
            return Ok(());
        }
        if helper != crate::mem_map::HELPER_DISPATCHER_BASE {
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

    #[cfg(feature = "from-bpf-toml")]
    pub fn from_bpf_toml(&mut self) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        if table::boot::BOOT_IMAGES.len() > MAX_IMAGES {
            return Err(XdpError::TooManyImages);
        }
        table::boot::load(self)
    }

    pub(crate) fn find_name(&self, name: &str) -> Option<XdpId> {
        self.images.iter().position(|img| img.meta.name == name)
    }

    /// Write image bytes into the cold table and register metadata via [`XdpBuilder`].
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
        self.table.erase(offset, self.table.image_capacity());
        self.table.write(offset, image);
        XdpBuilder::new()
            .with_name(name)
            .with_table_index(table_id)
            .with_image_len(image.len())
            .with_link_addr(0)
            .complete()?
            .build(self)
    }

    /// Register metadata for bytes already stored at `table_id`.
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
        required_maps: &[&str],
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
        self.table.erase(offset, self.table.image_capacity());
        self.table.write(offset, image);
        XdpBuilder::new()
            .with_name(name)
            .with_table_index(table_id)
            .with_image_len(image.len())
            .with_link_addr(link_addr)
            .with_helper_address(helper_address)
            .with_abi_version(abi_version)
            .with_required_maps(required_maps.iter().copied())
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
        self.table.erase(offset, cap);
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
            XdpLocation::Table { index } => {
                let _ = index;
                Err(XdpError::NotImplemented)
            }
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
        self.table.read(offset, &mut buf[..image_len]);
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

    /// Direct-start: treat primary slot bytes as already loaded (post-link inject).
    ///
    /// Sets `loaded_len` without cold-table copy; leaves `image` as `None` (no
    /// [`XdpMeta`]). Call before [`Self::lease`]`(None)` (or with hand-built maps).
    pub fn assume_injected(&mut self) -> Result<(), XdpError> {
        if self.leased {
            return Err(XdpError::Leased);
        }
        if self.primary == usize::MAX {
            return Err(XdpError::NoPrimarySlot);
        }
        let slot_state = self
            .slots
            .get_mut(self.primary)
            .ok_or(XdpError::NoPrimarySlot)?;
        slot_state.loaded_len = slot_state.slot.len();
        slot_state.image = None;
        Ok(())
    }

    pub fn pre_lease(&self, maps: Option<&MapRtGuard<'_>>) -> Result<(), XdpError> {
        let (image_id, loaded_len) = {
            let slot = self
                .slots
                .get(self.primary)
                .ok_or(XdpError::NoPrimarySlot)?;
            (slot.image, slot.loaded_len)
        };
        if loaded_len == 0 {
            return Err(XdpError::PrimaryEmpty);
        }
        let Some(id) = image_id else {
            // Firmware-first path: no meta row — only confirm slot readiness.
            return Ok(());
        };
        self.check(id, self.primary)?;
        let image = self.images.get(id).ok_or(XdpError::UnknownImage)?;
        image.meta.check_required_maps(maps)?;
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
            let mut binds = heapless::Vec::<(usize, crate::map::MapId), MAX_VMR_BINDS>::new();
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
                if self.vmr_binds.push(VmrBind { vma, map_id, rma }).is_err()
                    || binds.push((vma, map_id)).is_err()
                {
                    self.rollback_map_users(maps);
                    return Err(XdpError::MapLease);
                }
            }
            // Also lease required_maps by name if not already in AOT binds.
            if let Some(id) = self.slots.get(self.primary).and_then(|s| s.image) {
                let names: Vec<String> = self.images[id].meta.required_maps.clone();
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
            crate::map::set_map_aliases(&binds).map_err(|_| XdpError::MapLease)?;
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
        #[cfg(target_arch = "riscv32")]
        unsafe {
            core::arch::asm!("fence.i");
        }
    }
}
