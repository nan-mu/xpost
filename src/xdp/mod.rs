//! XDP execution runtime. Control-plane mutation is serialized by `XDP_RT`;
//! RX sees only the atomically published entry in `state`.

pub(crate) mod image;
pub(crate) mod ingress;
pub(crate) mod map;
mod ready;
pub(crate) mod slot;
mod state;
pub(crate) mod table;
// TODO: `pub(crate) mod faults` — sibling error counters for ELF collect / ingress / table
// (see plan XDP ELF faults-module; not implemented this phase).

pub use crate::error::{
    XdpInitError, XdpLeaseError, XdpReleaseError, XdpSlotError, XdpSwitchError,
};
pub use crate::{RtMutex, RtMutexGuard};
pub(crate) use image::XdpMeta;
pub use image::{Complete, Incomplete, XdpBuilder, XdpLocation, XdpMaps};
/// Locked [`XdpRt`].
pub type XdpRtGuard<'a> = RtMutexGuard<'a, XdpRt>;
pub use slot::{Slot, XdpSlot};
pub use state::XDP_RT;
pub use table::storage::ColdRegion;
pub(crate) use table::storage::{DEFAULT_IMAGE_CAPACITY, EspFlashXdpTable, XdpTableStorage};

use alloc::{string::String, vec::Vec};
use core::sync::atomic::Ordering;

use crate::{
    error::{MapCheckError, MapUseError},
    map::{MapBuilder, MapRtGuard, MapType},
    xdp::{
        map::MapLease,
        ready::ReadyError,
        state::{ACTIVE, IN_FLIGHT, MAX_IMAGES, MAX_SLOTS, PRIMARY_ENTRY},
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
    /// Lease-time VMR bindings + MapRt user refs (see [`map`]).
    pub(crate) map_lease: MapLease,
}

impl XdpRt {
    // ═══════════════════════════════════════════
    // boot
    // ═══════════════════════════════════════════

    /// Firmware-first boot: cold table + IRAM slots, scan XDP ELF, create maps.
    ///
    /// `flash` is the chip FLASH peripheral. Packed `xdp.elf` is written with
    /// `espflash write-bin <cold.flash_base> <xdp.elf>`. Empty / erased regions
    /// register nothing. [`crate::map::MapRt`] must already be initialized (heap claimed).
    ///
    /// `cold` is a firmware board constant `(flash_base, flash_len, image_capacity)`.
    /// `helper_expected` is the board-linked helper address (`0` skips the check).
    pub async fn init(
        flash: esp_hal::peripherals::FLASH<'static>,
        slots: &[XdpSlot],
        cold: impl Into<ColdRegion>,
        maps: &mut MapRtGuard<'_>,
        helper_expected: usize,
    ) -> Result<(XdpRtGuard<'static>, Vec<ReportItem>), XdpInitError> {
        Self::check_helper_address(helper_expected)?;
        let cold = cold.into();
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
            map_lease: MapLease::new(),
        };
        for slot in slots {
            rt.register_slot(*slot)?;
        }
        Self::inner_init(rt, maps).await
    }

    /// Install runtime into `XDP_RT` after slots / discover / maps are ready.
    async fn inner_init(
        mut rt: Self,
        maps: &mut MapRtGuard<'_>,
    ) -> Result<(XdpRtGuard<'static>, Vec<ReportItem>), XdpInitError> {
        if rt.primary == usize::MAX {
            return Err(XdpInitError::NoPrimarySlot);
        }
        let _ = rt.discover_elfs();
        let report = rt.create_required_maps(maps)?;
        if XDP_RT.init(RtMutex::new(rt)).is_err() {
            return Err(XdpInitError::AlreadyInitialized);
        }
        Ok((XDP_RT.get().await.lock().await, report))
    }

    /// Scan cold region; register each XDP ELF meta via [`Self::register_meta`].
    fn discover_elfs(&mut self) -> usize {
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

    /// Create missing `required_maps`; build boot report.
    fn create_required_maps(
        &self,
        maps: &mut MapRtGuard<'_>,
    ) -> Result<Vec<ReportItem>, XdpInitError> {
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
                    maps.check(spec)?;
                    continue;
                }
                match maps.find(&spec.name) {
                    Ok(_) => {
                        maps.check(spec)?;
                    }
                    Err(MapUseError::NotFound) => {
                        maps.create(MapBuilder::from_meta(spec))?;
                    }
                    Err(_) => {
                        return Err(XdpInitError::MapCheck(MapCheckError::TypeMismatch));
                    }
                }
                report.push(ReportItem::Map(spec.map_type, spec.name.clone()));
            }
        }
        Ok(report)
    }

    /// Register a validated, non-overlapping `XdpSlot` (init only).
    fn register_slot(&mut self, slot: XdpSlot) -> Result<(), XdpInitError> {
        slot.validate()?;
        let end = slot
            .base()
            .checked_add(slot.len())
            .ok_or(XdpInitError::SlotOverflow)?;
        if self.slots.iter().any(|old| {
            slot.base() < old.slot.base().saturating_add(old.slot.len()) && old.slot.base() < end
        }) {
            return Err(XdpInitError::OverlappingSlots);
        }
        if slot.is_primary() {
            if self.primary != usize::MAX {
                return Err(XdpInitError::MultiplePrimarySlots);
            }
            self.primary = self.slots.len();
        }
        self.slots
            .push(SlotState {
                slot,
                loaded_len: 0,
                image: None,
            })
            .map_err(|_| XdpInitError::TooManySlots)?;
        Ok(())
    }

    // ═══════════════════════════════════════════
    // inventory
    // ═══════════════════════════════════════════

    /// Lookup image id by program name.
    pub fn find(&self, name: &str) -> Option<XdpId> {
        self.images.iter().position(|img| img.meta.name == name)
    }

    /// Register metadata for bytes already in cold table (`discover_elfs` / `XdpBuilder`).
    pub(crate) fn register_meta(
        &mut self,
        meta: XdpMeta,
        table_id: XdpId,
    ) -> Result<XdpId, XdpInitError> {
        if self.leased {
            // Boot inventory only; lease must not register metas.
            return Err(XdpInitError::AlreadyInitialized);
        }
        if self.images.len() > table_id && self.images[table_id].table_id == table_id {
            return Err(XdpInitError::DuplicateName);
        }
        self.images
            .push(ImageRecord { meta, table_id })
            .map_err(|_| XdpInitError::TooManyImages)?;
        Ok(self.images.len() - 1)
    }

    // ═══════════════════════════════════════════
    // checks（相邻；helper 仅 init）
    // ═══════════════════════════════════════════

    /// Live `helper_dispatcher` vs firmware-passed expected link address (`0` = skip).
    ///
    /// Distinct from [`XdpMeta::check_helper_address`] (meta field vs live symbol).
    fn check_helper_address(expected: usize) -> Result<(), XdpInitError> {
        if expected == 0 {
            return Ok(());
        }
        let helper = crate::helper::helper_dispatcher as *const () as usize;
        if helper != expected {
            return Err(XdpInitError::LinkedAddress);
        }
        Ok(())
    }

    /// Target slot must have `len >= meta.image_len` (and image non-empty).
    fn check_image_size(&self, meta: &XdpMeta, slot: SlotId) -> Result<(), ReadyError> {
        if meta.image_len == 0 {
            return Err(ReadyError::ImageEmpty);
        }
        let slot_state = self.slots.get(slot).ok_or(ReadyError::UnknownSlot)?;
        if meta.image_len > slot_state.slot.len() {
            return Err(ReadyError::ImageTooLarge);
        }
        Ok(())
    }

    /// Every `meta.required_maps` entry exists in `MapRt` with matching type.
    fn check_required_maps(
        &self,
        meta: &XdpMeta,
        maps: Option<&MapRtGuard<'_>>,
    ) -> Result<(), ReadyError> {
        if meta.required_maps.is_empty() {
            return Ok(());
        }
        let Some(rt) = maps else {
            return Err(ReadyError::MapsRequired);
        };
        for req in &meta.required_maps {
            match rt.check(req) {
                Ok(_) => {}
                Err(MapCheckError::NotFound) => return Err(ReadyError::MissingMap),
                Err(e) => return Err(ReadyError::MapCheck(e)),
            }
        }
        Ok(())
    }

    // ═══════════════════════════════════════════
    // placement / lease
    // ═══════════════════════════════════════════

    /// Read-only readiness gate.
    ///
    /// - `id: Some(x)` → meta from inventory `images[x]` (switch / early check).
    /// - `id: None` → meta via primary slot's loaded image (lease path); requires
    ///   primary already loaded with `location == Slot { primary }`.
    ///
    /// Leaf checks: [`Self::check_image_size`], ABI/arch/helper/link, [`Self::check_required_maps`].
    pub(crate) fn pre_lease(
        &self,
        maps: Option<&MapRtGuard<'_>>,
        id: Option<XdpId>,
    ) -> Result<(), ReadyError> {
        if self.primary == usize::MAX {
            return Err(ReadyError::NoPrimarySlot);
        }
        let primary = self.primary;
        let slot_state = self.slots.get(primary).ok_or(ReadyError::NoPrimarySlot)?;

        let meta = match id {
            Some(id) => &self.images.get(id).ok_or(ReadyError::UnknownImage)?.meta,
            None => {
                if slot_state.loaded_len == 0 {
                    return Err(ReadyError::PrimaryEmpty);
                }
                let Some(loaded_id) = slot_state.image else {
                    return Err(ReadyError::PrimaryEmpty);
                };
                let meta = &self
                    .images
                    .get(loaded_id)
                    .ok_or(ReadyError::UnknownImage)?
                    .meta;
                match meta.location() {
                    XdpLocation::Slot { index } if index == primary => {}
                    _ => return Err(ReadyError::SlotMismatch),
                }
                meta
            }
        };

        self.check_image_size(meta, primary)?;
        meta.check_arch()?;
        meta.check_helper_address()?;
        meta.check_slot_address(&slot_state.slot)?;
        self.check_required_maps(meta, maps)?;
        Ok(())
    }

    /// Copy XDP binary of `id` from cold table into `slot`, set primary to `slot`,
    /// after validating via [`Self::pre_lease`].
    pub fn switch(
        &mut self,
        id: XdpId,
        slot: SlotId,
        maps: Option<&MapRtGuard<'_>>,
    ) -> Result<(), XdpSwitchError> {
        if self.leased {
            return Err(XdpSwitchError::Leased);
        }
        let _ = self.slots.get(slot).ok_or(XdpSwitchError::UnknownSlot)?;

        let prev_primary = self.primary;
        self.primary = slot;
        if let Err(e) = self.pre_lease(maps, Some(id)) {
            self.primary = prev_primary;
            return Err(e.into());
        }

        let (aot_abs, image_len, link_addr) = {
            let image = self.images.get(id).ok_or(XdpSwitchError::UnknownImage)?;
            match image.meta.location() {
                XdpLocation::Table { .. } => {}
                XdpLocation::Slot { .. } => {
                    // Already in a slot: still allow re-copy into target.
                }
            }
            (
                image.meta.aot_abs(),
                image.meta.image_len,
                image.meta.link_addr,
            )
        };
        let (dst, capacity, occupied) = {
            let slot_state = self.slots.get(slot).ok_or(XdpSwitchError::UnknownSlot)?;
            if slot_state.slot.base() != link_addr && link_addr != 0 {
                self.primary = prev_primary;
                return Err(XdpSwitchError::SlotMismatch);
            }
            (
                slot_state.slot.base() as *mut u8,
                slot_state.slot.len(),
                slot_state.image,
            )
        };
        if occupied.is_some() && occupied != Some(id) {
            self.primary = prev_primary;
            return Err(XdpSwitchError::SlotOccupied);
        }
        if image_len > capacity {
            self.primary = prev_primary;
            return Err(XdpSwitchError::ImageTooLarge);
        }
        let mut buf = [0u8; DEFAULT_IMAGE_CAPACITY];
        if image_len > buf.len() {
            self.primary = prev_primary;
            return Err(XdpSwitchError::ScratchTooSmall);
        }
        // Unaligned image_len is fine: EspFlashXdpTable::read pads like visit_bytes.
        if let Err(e) = self.table.read(aot_abs, &mut buf[..image_len]) {
            self.primary = prev_primary;
            return Err(e.into());
        }
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dst, image_len);
            core::ptr::write_bytes(dst.add(image_len), 0, capacity - image_len);
        }
        Self::fence_instruction_cache();
        let slot_state = self
            .slots
            .get_mut(slot)
            .ok_or(XdpSwitchError::UnknownSlot)?;
        slot_state.loaded_len = image_len;
        slot_state.image = Some(id);
        self.images
            .get_mut(id)
            .ok_or(XdpSwitchError::UnknownImage)?
            .meta
            .set_location(XdpLocation::Slot { index: slot });
        Ok(())
    }

    /// Enable the datapath: [`Self::pre_lease`], install VMR aliases / map users, publish entry.
    ///
    /// Closes `MapRt` control-plane access before `ACTIVE` is stored.
    ///
    /// VMR pairs come only from the primary image's runtime meta (`aot_binds`).
    /// Pass `None` only when the leased image has no `required_maps`.
    pub fn lease(&mut self, maps: Option<&mut MapRtGuard<'_>>) -> Result<(), XdpLeaseError> {
        if self.leased {
            return Err(XdpLeaseError::Leased);
        }
        let maps_ref = maps.as_deref();
        self.pre_lease(maps_ref, None)?;
        let entry = {
            let slot = self
                .slots
                .get(self.primary)
                .ok_or(XdpLeaseError::NoPrimarySlot)?;
            slot.slot.base()
        };
        self.map_lease.clear();
        let caller_holds_maps = maps.is_some();
        if let Some(maps) = maps {
            if let Some(id) = self.slots.get(self.primary).and_then(|s| s.image) {
                let binds: Vec<(String, usize)> =
                    self.images[id].meta.aot_binds.iter().cloned().collect();
                for (name, vma) in binds {
                    if vma == 0 {
                        continue;
                    }
                    if let Err(e) = self.bind_vmr(maps, &name, vma) {
                        self.rollback_map_users(maps);
                        return Err(e);
                    }
                }
                let names: Vec<String> = self.images[id]
                    .meta
                    .required_maps
                    .iter()
                    .map(|r| r.name.clone())
                    .collect();
                for name in names {
                    let Ok(map_id) = maps.find(&name) else {
                        self.rollback_map_users(maps);
                        return Err(XdpLeaseError::MissingMap);
                    };
                    if !self.map_lease.refs.contains(&map_id) {
                        if let Err(e) = maps.add_user(map_id) {
                            self.rollback_map_users(maps);
                            return Err(e.into());
                        }
                        if self.map_lease.refs.push(map_id).is_err() {
                            self.rollback_map_users(maps);
                            return Err(XdpLeaseError::AliasFull);
                        }
                    }
                }
            }
            let binds = self.map_lease.vmr.alias_pairs();
            if !binds.is_empty() {
                crate::map::set_map_aliases(binds.as_slice())?;
                crate::map::set_fast_arrays(maps, binds.as_slice());
            }
        } else if let Some(id) = self.slots.get(self.primary).and_then(|s| s.image)
            && !self.images[id].meta.required_maps.is_empty()
        {
            return Err(XdpLeaseError::MapsRequired);
        }
        // Close control-plane map access before any flight can observe `ACTIVE`.
        // Caller holds `MAP_RT` when `maps` is `Some`. Otherwise take it here so
        // a control-plane critical section cannot overlap this store.
        if caller_holds_maps {
            crate::map::close_ctrl();
        } else if let Some(lock) = crate::map::MAP_RT.try_get() {
            let _guard = lock
                .try_lock()
                .map_err(|_| XdpLeaseError::Map(MapUseError::Busy))?;
            crate::map::close_ctrl();
        } else {
            crate::map::close_ctrl();
        }
        PRIMARY_ENTRY.store(entry, Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        self.leased = true;
        Ok(())
    }

    /// Async wrapper around [`Self::lease`].
    pub async fn lease_async(
        &mut self,
        maps: Option<&mut MapRtGuard<'_>>,
    ) -> Result<(), XdpLeaseError> {
        self.lease(maps)
    }

    /// True while an image is leased.
    pub fn leased(&self) -> bool {
        self.leased
    }

    /// Disable datapath: clear aliases, drop map users, clear VMR table.
    pub fn release(&mut self) -> Result<(), XdpReleaseError> {
        if !self.leased {
            return Err(XdpReleaseError::NotLeased);
        }
        // SeqCst: Dekker pair with `ingress::run` (IN_FLIGHT claim → ACTIVE check).
        ACTIVE.store(false, Ordering::SeqCst);
        while IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            core::hint::spin_loop();
        }
        PRIMARY_ENTRY.store(0, Ordering::Release);
        crate::map::clear_map_aliases();
        for id in self.map_lease.refs.drain(..) {
            let _ = crate::map::remove_user_sync(id);
        }
        self.map_lease.vmr.clear();
        crate::map::open_ctrl();
        self.leased = false;
        Ok(())
    }

    // ═══════════════════════════════════════════
    // misc
    // ═══════════════════════════════════════════

    /// Resolve IRAM base address to a registered slot id.
    pub fn slot_for(&self, address: usize) -> Result<SlotId, XdpSlotError> {
        self.slots
            .iter()
            .position(|slot| slot.slot.base() == address)
            .ok_or(XdpSlotError::UnknownSlot)
    }

    /// `fence.i` after writing executable bytes into a slot.
    pub(crate) fn fence_instruction_cache() {
        unsafe {
            core::arch::asm!("fence.i");
        }
    }
}
