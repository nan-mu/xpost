//! XDP execution runtime. Control-plane mutation is serialized by `XDP_RT`;
//! RX sees only the atomically published entry in `state`.

#[cfg(feature = "debug")]
macro_rules! xdp_debug { ($($arg:tt)*) => { log::debug!($($arg)*); }; }
#[cfg(not(feature = "debug"))]
macro_rules! xdp_debug {
    ($($arg:tt)*) => {};
}

mod boot;
mod device;
mod slot;
mod state;
mod xdp_table;

pub use device::{Device, ZcTx};
pub use slot::Slot;
pub use state::XDP_RT;

use anyhow::{Result, bail};
use core::sync::atomic::Ordering;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};
use state::{ACTIVE, IN_FLIGHT, MAX_IMAGES, MAX_MAP_REFS, MAX_SLOTS, PRIMARY_ENTRY};
use xdp_table::XdpTable;

/// Index of an image's fixed cold-table entry.
pub type XdpTableId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpLocation {
    Table(XdpTableId),
    Slot(usize),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpMoveTarget {
    Table,
    Slot(usize),
}

/// Metadata is externally owned and mutably borrowed for every state change.
/// It has no interior mutability and needs no `Sync` implementation.
#[derive(Clone, Copy)]
pub struct XdpMeta {
    table_id: XdpTableId,
    link_addr: usize,
    image_len: usize,
    location: XdpLocation,
}
impl XdpMeta {
    pub const fn in_table(table_id: XdpTableId, link_addr: usize, image_len: usize) -> Self {
        Self {
            table_id,
            link_addr,
            image_len,
            location: XdpLocation::Table(table_id),
        }
    }
    pub const fn table_id(&self) -> XdpTableId {
        self.table_id
    }
    pub const fn link_addr(&self) -> usize {
        self.link_addr
    }
    pub const fn image_len(&self) -> usize {
        self.image_len
    }
    pub const fn locate(&self) -> XdpLocation {
        self.location
    }
}

#[derive(Clone, Copy)]
struct SlotMeta {
    slot: Slot,
    loaded_len: usize,
    image: Option<XdpTableId>,
}

pub struct XdpRt {
    table: XdpTable,
    slots: heapless::Vec<SlotMeta, MAX_SLOTS>,
    primary: usize,
    leased: bool,
    map_refs: heapless::Vec<crate::map::MapIdx, MAX_MAP_REFS>,
}

impl XdpRt {
    /// Register every linker-defined slot once at boot. There is deliberately
    /// no post-boot slot-registration API.
    pub async fn init(
        slots: &'static [Slot],
        table: &'static mut [u8],
        image_capacity: usize,
    ) -> Result<MutexGuard<'static, CriticalSectionRawMutex, XdpRt>> {
        let mut rt = Self {
            table: XdpTable::new(table, image_capacity)?,
            slots: heapless::Vec::new(),
            primary: usize::MAX,
            leased: false,
            map_refs: heapless::Vec::new(),
        };
        for slot in slots {
            rt.register_slot(*slot)?;
        }
        if rt.primary == usize::MAX {
            bail!("XDP slot registration has no primary slot");
        }
        XDP_RT
            .init(Mutex::new(rt))
            .map_err(|_| anyhow::anyhow!("XDP runtime already initialized"))?;
        Ok(XDP_RT.get().await.lock().await)
    }
    fn register_slot(&mut self, slot: Slot) -> Result<()> {
        slot.validate()?;
        let end = slot
            .base()
            .checked_add(slot.len())
            .ok_or_else(|| anyhow::anyhow!("XDP slot range overflows"))?;
        if self.slots.iter().any(|old| {
            slot.base() < old.slot.base().saturating_add(old.slot.len()) && old.slot.base() < end
        }) {
            bail!("XDP slots overlap");
        }
        if slot.is_primary() {
            if self.primary != usize::MAX {
                bail!("XDP has more than one primary slot");
            }
            self.primary = self.slots.len();
        }
        self.slots
            .push(SlotMeta {
                slot,
                loaded_len: 0,
                image: None,
            })
            .map_err(|_| anyhow::anyhow!("too many XDP slots"))?;
        Ok(())
    }
    pub fn from_bpf_toml(&mut self) -> Result<()> {
        if self.leased {
            bail!("XDP runtime is leased");
        }
        if boot::BOOT_IMAGES.len() > MAX_IMAGES {
            bail!("bpf.toml declares more XDP images than the runtime supports");
        }
        boot::load(self)
    }

    /// Save external bytes (for example a completed network download) into
    /// the table item declared by `meta`.
    pub fn save(&mut self, meta: &mut XdpMeta, bytes: &[u8]) -> Result<()> {
        if self.leased {
            bail!("XDP runtime is leased");
        }
        if bytes.len() != meta.image_len {
            bail!("XDP image length differs from its metadata");
        }
        self.table
            .save(meta.table_id, bytes.as_ptr(), bytes.len())?;
        meta.location = XdpLocation::Table(meta.table_id);
        Ok(())
    }
    /// Load an image from its table item into one registered slot.
    pub fn load(&mut self, meta: &mut XdpMeta, target: usize) -> Result<()> {
        self.move_image(meta, XdpMoveTarget::Slot(target))
    }
    pub fn move_image(&mut self, meta: &mut XdpMeta, target: XdpMoveTarget) -> Result<()> {
        if self.leased {
            bail!("XDP runtime is leased");
        }
        match target {
            XdpMoveTarget::Table => self.move_to_table(meta),
            XdpMoveTarget::Slot(slot) => self.move_to_slot(meta, slot),
        }
    }
    fn move_to_slot(&mut self, meta: &mut XdpMeta, target: usize) -> Result<()> {
        let slot = self
            .slots
            .get(target)
            .ok_or_else(|| anyhow::anyhow!("unknown XDP slot"))?;
        if slot.slot.base() != meta.link_addr {
            bail!("XDP image link address does not match target slot");
        }
        if meta.location == XdpLocation::Slot(target) {
            return Ok(());
        }
        if slot.image.is_some() {
            bail!("target XDP slot is occupied");
        }
        self.table.load(
            meta.table_id,
            slot.slot.base() as *mut u8,
            meta.image_len,
            slot.slot.len(),
        )?;
        let slot = self.slots.get_mut(target).unwrap();
        slot.loaded_len = meta.image_len;
        slot.image = Some(meta.table_id);
        Self::fence_instruction_cache();
        meta.location = XdpLocation::Slot(target);
        xdp_debug!("[xdp][load] table={} slot={}", meta.table_id, target);
        Ok(())
    }
    fn move_to_table(&mut self, meta: &mut XdpMeta) -> Result<()> {
        let XdpLocation::Slot(source) = meta.location else {
            return Ok(());
        };
        if source == self.primary {
            bail!("cannot move primary image while it is selected");
        }
        let slot = self
            .slots
            .get(source)
            .ok_or_else(|| anyhow::anyhow!("unknown XDP slot"))?;
        if slot.image != Some(meta.table_id) || slot.loaded_len != meta.image_len {
            bail!("XDP metadata and slot disagree");
        }
        self.table.save(
            meta.table_id,
            slot.slot.base() as *const u8,
            slot.loaded_len,
        )?;
        let slot = self.slots.get_mut(source).unwrap();
        slot.loaded_len = 0;
        slot.image = None;
        meta.location = XdpLocation::Table(meta.table_id);
        Ok(())
    }
    /// Select a stopped candidate; the caller leases it afterwards.
    pub fn switch(&mut self, meta: &mut XdpMeta) -> Result<()> {
        if self.leased {
            bail!("release XDP before switching");
        }
        let target = self.slot_for(meta.link_addr)?;
        self.load(meta, target)?;
        self.primary = target;
        Ok(())
    }
    /// Publish a candidate in an inactive slot without stopping RX.
    pub fn hot_switch(&mut self, meta: &mut XdpMeta) -> Result<()> {
        if !self.leased {
            bail!("XDP is not leased");
        }
        if IN_FLIGHT.load(Ordering::Acquire) != 0 {
            bail!("an XDP invocation is still in flight");
        }
        let target = self.slot_for(meta.link_addr)?;
        if target == self.primary {
            bail!("hot switch requires an inactive slot");
        }
        if meta.location == XdpLocation::Table(meta.table_id) {
            self.load_while_leased(meta, target)?;
        }
        if meta.location != XdpLocation::Slot(target) {
            bail!("candidate is not in its linked slot");
        }
        let slot = self.slots.get(target).unwrap();
        PRIMARY_ENTRY.store(slot.slot.base(), Ordering::Release);
        self.primary = target;
        Ok(())
    }
    fn load_while_leased(&mut self, meta: &mut XdpMeta, target: usize) -> Result<()> {
        let slot = self
            .slots
            .get(target)
            .ok_or_else(|| anyhow::anyhow!("unknown XDP slot"))?;
        if slot.slot.base() != meta.link_addr || slot.image.is_some() {
            bail!("hot-switch target is unavailable");
        }
        self.table.load(
            meta.table_id,
            slot.slot.base() as *mut u8,
            meta.image_len,
            slot.slot.len(),
        )?;
        let slot = self.slots.get_mut(target).unwrap();
        slot.loaded_len = meta.image_len;
        slot.image = Some(meta.table_id);
        Self::fence_instruction_cache();
        meta.location = XdpLocation::Slot(target);
        Ok(())
    }
    pub(super) fn slot_for(&self, address: usize) -> Result<usize> {
        self.slots
            .iter()
            .position(|slot| slot.slot.base() == address)
            .ok_or_else(|| anyhow::anyhow!("no XDP slot matches image link address"))
    }
    fn fence_instruction_cache() {
        #[cfg(target_arch = "riscv32")]
        unsafe {
            core::arch::asm!("fence.i");
        }
    }
    pub async fn lease(&mut self, maps: Option<&crate::map::MapRt>) -> Result<()> {
        if self.leased {
            bail!("XDP is already leased");
        }
        let slot = self
            .slots
            .get(self.primary)
            .ok_or_else(|| anyhow::anyhow!("no primary XDP slot"))?;
        if slot.loaded_len == 0 {
            bail!("primary XDP slot has no image");
        }
        if maps.is_some() {
            crate::map::bind_aot_map_addrs();
            for name in crate::map::AOT_MAP_USER_NAMES {
                let id = crate::map::resolve_map_name(name)
                    .ok_or_else(|| anyhow::anyhow!("required XDP map is absent"))?;
                if self.map_refs.is_full() || !crate::map::pool::RawResource::add_user_sync(id) {
                    self.release_map_refs();
                    bail!("cannot lease required XDP map");
                }
                let _ = self.map_refs.push(id);
            }
        }
        PRIMARY_ENTRY.store(slot.slot.base(), Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        self.leased = true;
        Ok(())
    }
    pub async fn release(&mut self) -> Result<()> {
        if !self.leased {
            bail!("XDP is not leased");
        }
        ACTIVE.store(false, Ordering::Release);
        while IN_FLIGHT.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
        PRIMARY_ENTRY.store(0, Ordering::Release);
        self.release_map_refs();
        self.leased = false;
        Ok(())
    }
    fn release_map_refs(&mut self) {
        for id in self.map_refs.drain(..) {
            let _ = crate::map::runtime::remove_user_sync(id);
        }
    }
}

#[inline]
pub(crate) fn primary_entry() -> Option<usize> {
    if !ACTIVE.load(Ordering::Acquire) {
        return None;
    }
    let entry = PRIMARY_ENTRY.load(Ordering::Acquire);
    (entry != 0).then_some(entry)
}
#[inline]
pub(crate) fn is_leased() -> bool {
    ACTIVE.load(Ordering::Acquire)
}
#[inline]
pub(crate) fn with_primary<R>(f: impl FnOnce(usize) -> R) -> Option<R> {
    let entry = primary_entry()?;
    IN_FLIGHT.fetch_add(1, Ordering::Acquire);
    if !ACTIVE.load(Ordering::Acquire) || PRIMARY_ENTRY.load(Ordering::Acquire) != entry {
        IN_FLIGHT.fetch_sub(1, Ordering::Release);
        return None;
    }
    let r = f(entry);
    IN_FLIGHT.fetch_sub(1, Ordering::Release);
    Some(r)
}
pub(crate) fn run_ingress(data: &mut [u8]) -> Option<i32> {
    with_primary(|entry| {
        crate::redirect::clear_pending();
        invoke_entry(entry, data)
    })
}
#[cfg(not(target_arch = "riscv32"))]
fn invoke_entry(_: usize, _: &mut [u8]) -> i32 {
    crate::redirect::XDP_PASS
}
#[cfg(target_arch = "riscv32")]
fn invoke_entry(entry: usize, data: &mut [u8]) -> i32 {
    use crate::ctx::{self, XdpContext, XdpMd};
    crate::redirect::clear_pending();
    crate::prog_table::reset_tail_depth();
    let mut ctx = XdpMd::from_buf(data);
    let ctx_ptr = &mut ctx as *mut XdpContext;
    let frame = core::ptr::addr_of_mut!(crate::prog_table::XDP_FRAME);
    let result: i32;
    unsafe {
        core::arch::asm!("fence.i", "mv t0, sp", "lla t1, 1f", "sw t0, 0({frame})", "sw t1, 4({frame})", "sw s0, 8({frame})", "sw s1, 12({frame})", "sw s2, 16({frame})", "sw s3, 20({frame})", "sw s4, 24({frame})", "sw s5, 28({frame})", "sw s6, 32({frame})", "sw s7, 36({frame})", "sw s8, 40({frame})", "sw s9, 44({frame})", "sw s10, 48({frame})", "sw s11, 52({frame})", "jalr {entry}", "1:", frame = in(reg) frame, entry = in(reg) entry, in("a0") ctx_ptr, lateout("a0") result, out("t0") _, out("t1") _, clobber_abi("C"));
    }
    ctx::record_window(&ctx);
    result
}
