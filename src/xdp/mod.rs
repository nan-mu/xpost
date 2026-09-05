//! XDP runtime ownership, image movement, and RX execution state.

#[cfg(feature = "debug")]
macro_rules! xdp_debug { ($($arg:tt)*) => { log::debug!($($arg)*); }; }
#[cfg(not(feature = "debug"))]
macro_rules! xdp_debug {
    ($($arg:tt)*) => {};
}

mod device;
mod global;
mod xdp_table;

pub use device::{Device, ZcTx};
pub use global::XDP_RT;
pub use xdp_table::{XdpTable, XdpTableError, XdpTableId};

use core::cell::UnsafeCell;
use core::sync::atomic::Ordering;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};
use global::{ACTIVE, IN_FLIGHT, PRIMARY_ENTRY};
use xdp_table::XdpTableHandle;

const MAX_SLOTS: usize = 4;
const MAX_MAP_REFS: usize = 16;
const NO_SLOT: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpLocation {
    Table(XdpTableId),
    Slot(usize),
}

/// Destination selected for [`XdpRt::move_image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpMoveTarget {
    Table,
    Slot(usize),
}

/// Static metadata for one XDP image. The table id never changes; location
/// changes only through the globally serialized XDP runtime.
pub struct XdpMeta {
    table_id: XdpTableId,
    link_addr: usize,
    image_len: usize,
    location: UnsafeCell<XdpLocation>,
}
unsafe impl Sync for XdpMeta {}
impl XdpMeta {
    pub const fn in_table(table_id: XdpTableId, link_addr: usize, image_len: usize) -> Self {
        Self {
            table_id,
            link_addr,
            image_len,
            location: UnsafeCell::new(XdpLocation::Table(table_id)),
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
    pub fn locate(&self) -> XdpLocation {
        unsafe { *self.location.get() }
    }
    fn relocate(&self, location: XdpLocation) {
        unsafe {
            *self.location.get() = location;
        }
    }
}

/// Generated bpf.toml entry. Its static byte slice is materialized into the
/// cold-table item denoted by `meta.table_id()` during boot.
#[doc(hidden)]
pub struct BpfTomlImage {
    pub meta: &'static XdpMeta,
    pub bytes: &'static [u8],
}
mod generated {
    include!(concat!(env!("OUT_DIR"), "/xdp_bpf.rs"));
}

#[repr(align(4))]
pub struct Slot<const N: usize>(UnsafeCell<[u8; N]>);
unsafe impl<const N: usize> Sync for Slot<N> {}
impl<const N: usize> Slot<N> {
    pub const fn new(bytes: [u8; N]) -> Self {
        Self(UnsafeCell::new(bytes))
    }
    fn as_ptr(&self) -> *mut u8 {
        self.0.get().cast()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    AlreadyInitialized,
    EmptySlot,
    UnalignedSlot,
    OverlappingSlot,
    SlotLimit,
    Busy,
    NotLoaded,
    AlreadyLeased,
    NotLeased,
    ImageTooLarge,
    Table(XdpTableError),
    MapNotFound,
    MapRefLimit,
    NoInactiveSlot,
    AddressMismatch,
    SlotOccupied,
}
impl From<XdpTableError> for Error {
    fn from(value: XdpTableError) -> Self {
        Self::Table(value)
    }
}

#[derive(Clone, Copy)]
struct SlotMeta {
    ptr: *mut u8,
    len: usize,
    loaded_len: usize,
    image: Option<XdpTableId>,
}
unsafe impl Send for SlotMeta {}

pub struct XdpRt {
    table: XdpTableHandle,
    slots: heapless::Vec<SlotMeta, MAX_SLOTS>,
    primary: usize,
    leased: bool,
    map_refs: heapless::Vec<crate::map::MapIdx, MAX_MAP_REFS>,
}

impl XdpRt {
    pub async fn init<const N: usize, const IMAGES: usize, const IMAGE_CAPACITY: usize>(
        slot: &'static Slot<N>,
        table: &'static XdpTable<IMAGES, IMAGE_CAPACITY>,
    ) -> Result<MutexGuard<'static, CriticalSectionRawMutex, XdpRt>, Error> {
        let mut rt = Self {
            table: table.handle(),
            slots: heapless::Vec::new(),
            primary: NO_SLOT,
            leased: false,
            map_refs: heapless::Vec::new(),
        };
        rt.push_slot(slot, true)?;
        XDP_RT
            .init(Mutex::new(rt))
            .map_err(|_| Error::AlreadyInitialized)?;
        Ok(XDP_RT.get().await.lock().await)
    }

    pub fn push_slot<const N: usize>(
        &mut self,
        slot: &'static Slot<N>,
        primary: bool,
    ) -> Result<usize, Error> {
        if self.leased {
            return Err(Error::Busy);
        }
        if N == 0 {
            return Err(Error::EmptySlot);
        }
        let ptr = slot.as_ptr();
        if (ptr as usize) & 3 != 0 || N & 3 != 0 {
            return Err(Error::UnalignedSlot);
        }
        let end = (ptr as usize)
            .checked_add(N)
            .ok_or(Error::OverlappingSlot)?;
        if self.slots.iter().any(|old| {
            (ptr as usize) < (old.ptr as usize).saturating_add(old.len) && (old.ptr as usize) < end
        }) {
            return Err(Error::OverlappingSlot);
        }
        let id = self.slots.len();
        self.slots
            .push(SlotMeta {
                ptr,
                len: N,
                loaded_len: 0,
                image: None,
            })
            .map_err(|_| Error::SlotLimit)?;
        if primary {
            self.primary = id;
        }
        Ok(id)
    }

    /// Install generated static images in their table entries, then move them
    /// to the slot whose real base equals each image's link address.
    pub fn from_bpf_toml(&mut self) -> Result<(), Error> {
        if self.leased {
            return Err(Error::Busy);
        }
        for image in generated::BOOT_IMAGES {
            if image.bytes.len() != image.meta.image_len() {
                return Err(Error::ImageTooLarge);
            }
            self.table.copy_in(
                image.meta.table_id(),
                image.bytes.as_ptr(),
                image.bytes.len(),
            )?;
            image
                .meta
                .relocate(XdpLocation::Table(image.meta.table_id()));
            self.move_image(
                image.meta,
                XdpMoveTarget::Slot(self.slot_for(image.meta.link_addr())?),
            )?;
        }
        Ok(())
    }

    /// Move `meta` between its designated cold-table position and a slot.
    pub fn move_image(&mut self, meta: &XdpMeta, target: XdpMoveTarget) -> Result<(), Error> {
        if self.leased {
            return Err(Error::Busy);
        }
        match target {
            XdpMoveTarget::Table => self.move_to_table(meta),
            XdpMoveTarget::Slot(slot) => self.move_to_slot(meta, slot),
        }
    }

    fn move_to_slot(&mut self, meta: &XdpMeta, target: usize) -> Result<(), Error> {
        let slot = self.slots.get(target).ok_or(Error::NotLoaded)?;
        if slot.ptr as usize != meta.link_addr() {
            return Err(Error::AddressMismatch);
        }
        if let XdpLocation::Slot(current) = meta.locate() {
            return if current == target {
                Ok(())
            } else {
                Err(Error::SlotOccupied)
            };
        }
        if slot.image.is_some() {
            return Err(Error::SlotOccupied);
        }
        let len = self.table.copy_out(meta.table_id(), slot.ptr, slot.len)?;
        let slot = self.slots.get_mut(target).ok_or(Error::NotLoaded)?;
        slot.loaded_len = len;
        slot.image = Some(meta.table_id());
        Self::fence_instruction_cache();
        meta.relocate(XdpLocation::Slot(target));
        xdp_debug!(
            "[xdp][move] table={} -> slot={} bytes={}",
            meta.table_id().index(),
            target,
            len
        );
        Ok(())
    }

    fn move_to_table(&mut self, meta: &XdpMeta) -> Result<(), Error> {
        let XdpLocation::Slot(source) = meta.locate() else {
            return Ok(());
        };
        if source == self.primary {
            return Err(Error::Busy);
        }
        let slot = self.slots.get(source).ok_or(Error::NotLoaded)?;
        if slot.image != Some(meta.table_id()) || slot.loaded_len != meta.image_len() {
            return Err(Error::NotLoaded);
        }
        self.table
            .copy_in(meta.table_id(), slot.ptr, slot.loaded_len)?;
        let slot = self.slots.get_mut(source).ok_or(Error::NotLoaded)?;
        slot.loaded_len = 0;
        slot.image = None;
        meta.relocate(XdpLocation::Table(meta.table_id()));
        xdp_debug!(
            "[xdp][move] slot={} -> table={}",
            source,
            meta.table_id().index()
        );
        Ok(())
    }

    /// Set a stopped candidate as primary. A cold candidate is moved to its
    /// linked slot automatically; call `lease` afterward to resume RX.
    pub fn switch(&mut self, meta: &XdpMeta) -> Result<(), Error> {
        if self.leased {
            return Err(Error::Busy);
        }
        let target = self.slot_for(meta.link_addr())?;
        self.move_image(meta, XdpMoveTarget::Slot(target))?;
        self.primary = target;
        xdp_debug!("[xdp][switch] primary slot={}", target);
        Ok(())
    }

    /// Atomically publish an inactive candidate while RX remains leased.
    pub fn hot_switch(&mut self, meta: &XdpMeta) -> Result<(), Error> {
        if !self.leased {
            return Err(Error::NotLeased);
        }
        if IN_FLIGHT.load(Ordering::Acquire) != 0 {
            return Err(Error::Busy);
        }
        let target = self.slot_for(meta.link_addr())?;
        if target == self.primary {
            return Err(Error::NoInactiveSlot);
        }
        match meta.locate() {
            XdpLocation::Table(_) => self.move_to_slot_while_leased(meta, target)?,
            XdpLocation::Slot(slot) if slot == target => {}
            XdpLocation::Slot(_) => return Err(Error::SlotOccupied),
        }
        let slot = self.slots.get(target).ok_or(Error::NotLoaded)?;
        PRIMARY_ENTRY.store(slot.ptr as usize, Ordering::Release);
        self.primary = target;
        xdp_debug!("[xdp][hot-switch] primary slot={}", target);
        Ok(())
    }

    fn move_to_slot_while_leased(&mut self, meta: &XdpMeta, target: usize) -> Result<(), Error> {
        let slot = self.slots.get(target).ok_or(Error::NotLoaded)?;
        if slot.ptr as usize != meta.link_addr() {
            return Err(Error::AddressMismatch);
        }
        if slot.image.is_some() {
            return Err(Error::SlotOccupied);
        }
        let len = self.table.copy_out(meta.table_id(), slot.ptr, slot.len)?;
        let slot = self.slots.get_mut(target).ok_or(Error::NotLoaded)?;
        slot.loaded_len = len;
        slot.image = Some(meta.table_id());
        Self::fence_instruction_cache();
        meta.relocate(XdpLocation::Slot(target));
        Ok(())
    }
    fn slot_for(&self, link_addr: usize) -> Result<usize, Error> {
        self.slots
            .iter()
            .position(|slot| slot.ptr as usize == link_addr)
            .ok_or(Error::AddressMismatch)
    }
    fn fence_instruction_cache() {
        #[cfg(target_arch = "riscv32")]
        unsafe {
            core::arch::asm!("fence.i");
        }
    }

    pub async fn lease(&mut self, maps: Option<&crate::map::MapRt>) -> Result<(), Error> {
        if self.leased {
            return Err(Error::AlreadyLeased);
        }
        let slot = self.slots.get(self.primary).ok_or(Error::NotLoaded)?;
        if slot.loaded_len == 0 {
            return Err(Error::NotLoaded);
        }
        if maps.is_some() {
            crate::map::bind_aot_map_addrs();
            for name in crate::map::AOT_MAP_USER_NAMES {
                let id = crate::map::resolve_map_name(name).ok_or(Error::MapNotFound)?;
                if self.map_refs.is_full() {
                    self.release_map_refs();
                    return Err(Error::MapRefLimit);
                }
                if !crate::map::pool::RawResource::add_user_sync(id) {
                    self.release_map_refs();
                    return Err(Error::MapNotFound);
                }
                let _ = self.map_refs.push(id);
            }
        }
        PRIMARY_ENTRY.store(slot.ptr as usize, Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        self.leased = true;
        xdp_debug!(
            "[xdp][lease] active slot={} entry={}",
            self.primary,
            slot.ptr as usize
        );
        Ok(())
    }
    pub async fn release(&mut self) -> Result<(), Error> {
        if !self.leased {
            return Err(Error::NotLeased);
        }
        ACTIVE.store(false, Ordering::Release);
        while IN_FLIGHT.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
        PRIMARY_ENTRY.store(0, Ordering::Release);
        self.release_map_refs();
        self.leased = false;
        xdp_debug!("[xdp][release] complete");
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
    let result = f(entry);
    IN_FLIGHT.fetch_sub(1, Ordering::Release);
    Some(result)
}
pub(crate) fn run_ingress(data: &mut [u8]) -> Option<i32> {
    with_primary(|entry| {
        crate::redirect::clear_pending();
        let action = invoke_entry(entry, data);
        xdp_debug!(
            "[xdp][ingress] entry={} bytes={} action={}",
            entry,
            data.len(),
            action
        );
        action
    })
}
#[cfg(not(target_arch = "riscv32"))]
fn invoke_entry(_entry: usize, _data: &mut [u8]) -> i32 {
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
