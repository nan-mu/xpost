//! XDP runtime ownership and execution state.
//!
//! This module is the only owner of XDP slot registration and the RX execution
//! snapshot. No other module owns an XDP activity switch or executable entry.

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

mod device;
mod global;
mod xdp_table;

pub use device::{Device, ZcTx};
pub use global::XDP_RT;
pub use xdp_table::{XdpTable, XdpTableError};

use core::cell::UnsafeCell;
use core::sync::atomic::Ordering;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};
use global::{ACTIVE, IN_FLIGHT, PRIMARY_ENTRY};
use xdp_table::XdpTableHandle;

/// Maximum slots represented by the first XdpRt implementation.
const MAX_SLOTS: usize = 4;
const MAX_MAP_REFS: usize = 16;
const NO_SLOT: usize = usize::MAX;

/// A statically allocated executable slot. This is the stable-Rust equivalent
/// of the `SyncUnsafeCell<[u8; N]>` shown in the design document.
#[repr(align(4))]
pub struct Slot<const N: usize>(UnsafeCell<[u8; N]>);

unsafe impl<const N: usize> Sync for Slot<N> {}

impl<const N: usize> Slot<N> {
    pub const fn new(bytes: [u8; N]) -> Self {
        Self(UnsafeCell::new(bytes))
    }

    fn as_ptr(&self) -> *mut u8 {
        self.0.get().cast::<u8>()
    }
}

/// XDP runtime errors.
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
}

impl From<XdpTableError> for Error {
    fn from(error: XdpTableError) -> Self {
        Self::Table(error)
    }
}

#[derive(Clone, Copy)]
struct SlotMeta {
    ptr: *mut u8,
    len: usize,
    loaded_len: usize,
}

unsafe impl Send for SlotMeta {}

/// Manages registered executable slots and their lease state.
pub struct XdpRt {
    table: XdpTableHandle,
    slots: heapless::Vec<SlotMeta, MAX_SLOTS>,
    primary: usize,
    leased: bool,
    map_refs: heapless::Vec<crate::map::MapIdx, MAX_MAP_REFS>,
}

impl XdpRt {
    /// Register the first slot and publish the runtime singleton.
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

    /// Register another executable slot. Slots are immutable in identity for
    /// the lifetime of this runtime; only their image bytes may change.
    pub fn push_slot<const N: usize>(
        &mut self,
        slot: &'static Slot<N>,
        is_prim: bool,
    ) -> Result<usize, Error> {
        if self.leased {
            return Err(Error::Busy);
        }
        if N == 0 {
            return Err(Error::EmptySlot);
        }
        let ptr = slot.as_ptr();
        if (ptr as usize) & 0b11 != 0 || N & 0b11 != 0 {
            return Err(Error::UnalignedSlot);
        }
        let end = (ptr as usize)
            .checked_add(N)
            .ok_or(Error::OverlappingSlot)?;
        if self.slots.iter().any(|existing| {
            let existing_start = existing.ptr as usize;
            let existing_end = existing_start.saturating_add(existing.len);
            (ptr as usize) < existing_end && existing_start < end
        }) {
            return Err(Error::OverlappingSlot);
        }
        let id = self.slots.len();
        self.slots
            .push(SlotMeta {
                ptr,
                len: N,
                loaded_len: 0,
            })
            .map_err(|_| Error::SlotLimit)?;
        if is_prim {
            self.primary = id;
        }
        Ok(id)
    }

    /// Copy an image into one fixed table position.
    pub fn write_table_image(&mut self, image: usize, bytes: &[u8]) -> Result<(), Error> {
        if self.leased {
            xdp_debug!("[xdp][table] write rejected: runtime is leased image={}", image);
            return Err(Error::Busy);
        }
        self.table.copy_from_slot(image, bytes.as_ptr(), bytes.len())?;
        xdp_debug!("[xdp][table] wrote image={} bytes={}", image, bytes.len());
        Ok(())
    }

    /// Copy the stopped primary image into one table position.
    pub fn save_primary_to_table(&mut self, image: usize) -> Result<(), Error> {
        if self.leased {
            xdp_debug!("[xdp][table] save rejected: runtime is leased image={}", image);
            return Err(Error::Busy);
        }
        let slot = self.slots.get(self.primary).ok_or(Error::NotLoaded)?;
        self.table
            .copy_from_slot(image, slot.ptr.cast_const(), slot.loaded_len)?;
        xdp_debug!("[xdp][table] saved primary slot={} to image={}", self.primary, image);
        Ok(())
    }

    /// Copy a table image into the stopped primary executable slot.
    pub fn load_primary_from_table(&mut self, image: usize) -> Result<(), Error> {
        if self.leased {
            xdp_debug!("[xdp][table] load rejected: runtime is leased image={}", image);
            return Err(Error::Busy);
        }
        let slot = self.slots.get_mut(self.primary).ok_or(Error::NotLoaded)?;
        slot.loaded_len = self.table.copy_to_slot(image, slot.ptr, slot.len)?;
        Self::fence_instruction_cache();
        xdp_debug!("[xdp][table] loaded image={} into primary slot={}", image, self.primary);
        Ok(())
    }

    /// Exchange the stopped primary image with a table position.
    pub fn switch_primary_with_table(&mut self, image: usize) -> Result<(), Error> {
        if self.leased {
            xdp_debug!("[xdp][switch] rejected: runtime is leased image={}", image);
            return Err(Error::Busy);
        }
        let slot = self.slots.get_mut(self.primary).ok_or(Error::NotLoaded)?;
        slot.loaded_len = self
            .table
            .swap_with_slot(image, slot.ptr, slot.len, slot.loaded_len)?;
        Self::fence_instruction_cache();
        xdp_debug!("[xdp][switch] exchanged primary slot={} with image={}", self.primary, image);
        Ok(())
    }

    /// Publish a table image through an unused registered slot while RX stays
    /// leased. The old primary is never overwritten by this operation.
    pub fn hot_swap_primary_from_table(&mut self, image: usize) -> Result<(), Error> {
        if !self.leased {
            return self.load_primary_from_table(image);
        }
        if IN_FLIGHT.load(Ordering::Acquire) != 0 {
            xdp_debug!("[xdp][hot-swap] deferred: previous ingress call still active");
            return Err(Error::Busy);
        }
        let target = self
            .slots
            .iter()
            .enumerate()
            .find_map(|(index, _)| (index != self.primary).then_some(index))
            .ok_or(Error::NoInactiveSlot)?;
        let slot = &mut self.slots[target];
        slot.loaded_len = self.table.copy_to_slot(image, slot.ptr, slot.len)?;
        Self::fence_instruction_cache();
        PRIMARY_ENTRY.store(slot.ptr as usize, Ordering::Release);
        self.primary = target;
        xdp_debug!("[xdp][hot-swap] active slot={} image={}", target, image);
        Ok(())
    }

    fn fence_instruction_cache() {
        #[cfg(target_arch = "riscv32")]
        unsafe {
            core::arch::asm!("fence.i");
        }
    }

    /// Enable the primary slot.
    ///
    /// Passing a map runtime binds the generated AOT aliases and holds every
    /// referenced map until [`Self::release`].  The reference is an ownership
    /// lease, not a second runtime or a public raw-resource API.
    pub async fn lease(&mut self, maps: Option<&crate::map::MapRt>) -> Result<(), Error> {
        if self.leased {
            xdp_debug!("[xdp][lease] rejected: primary already leased");
            return Err(Error::AlreadyLeased);
        }
        let slot = match self.slots.get(self.primary) {
            Some(slot) => slot,
            None => {
                xdp_debug!("[xdp][lease] rejected: no primary slot registered");
                return Err(Error::NotLoaded);
            }
        };
        if slot.loaded_len == 0 {
            xdp_debug!("[xdp][lease] rejected: primary slot {} has no image", self.primary);
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
                // The ref is recorded only after the increment succeeds, so
                // rollback cannot release somebody else's existing lease.
                let _ = self.map_refs.push(id);
            }
        }
        PRIMARY_ENTRY.store(slot.ptr as usize, Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        self.leased = true;
        xdp_debug!(
            "[xdp][lease] active: slot={} entry={} image_bytes={} maps={}",
            self.primary,
            slot.ptr as usize,
            slot.loaded_len,
            maps.is_some()
        );
        Ok(())
    }

    /// Prevent new executions and wait for an in-progress RX invocation.
    pub async fn release(&mut self) -> Result<(), Error> {
        if !self.leased {
            xdp_debug!("[xdp][release] rejected: runtime is not leased");
            return Err(Error::NotLeased);
        }
        ACTIVE.store(false, Ordering::Release);
        xdp_debug!("[xdp][release] waiting for {} ingress calls", IN_FLIGHT.load(Ordering::Acquire));
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

/// Returns the leased primary entry for the RX hot path.
#[inline]
pub(crate) fn primary_entry() -> Option<usize> {
    if !ACTIVE.load(Ordering::Acquire) {
        xdp_debug!("[xdp][ingress] bypass: no active lease");
        return None;
    }
    let entry = PRIMARY_ENTRY.load(Ordering::Acquire);
    if entry == 0 {
        xdp_debug!("[xdp][ingress] bypass: active lease has no published entry");
        return None;
    }
    Some(entry)
}

/// Whether a primary slot is currently leased. Map control-plane code uses
/// this to reject writes that would race an AOT helper.
#[inline]
pub(crate) fn is_leased() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

/// Run `f` only while the leased primary snapshot remains active.
#[inline]
pub(crate) fn with_primary<R>(f: impl FnOnce(usize) -> R) -> Option<R> {
    let entry = primary_entry()?;
    IN_FLIGHT.fetch_add(1, Ordering::Acquire);
    if !ACTIVE.load(Ordering::Acquire) || PRIMARY_ENTRY.load(Ordering::Acquire) != entry {
        IN_FLIGHT.fetch_sub(1, Ordering::Release);
        xdp_debug!("[xdp][ingress] bypass: lease changed while acquiring snapshot");
        return None;
    }
    let result = f(entry);
    IN_FLIGHT.fetch_sub(1, Ordering::Release);
    Some(result)
}

/// Invoke the currently leased primary image for one RX frame.
///
/// This is the synchronous ingress fast path. `None` means no complete lease
/// was available, so its caller must pass the original frame through.
///
/// It is intentionally crate-private: the public injection point is
/// [`Device`], so callers cannot bypass the token and DMA lifetime rules.
pub(crate) fn run_ingress(data: &mut [u8]) -> Option<i32> {
    with_primary(|entry| {
        crate::redirect::clear_pending();
        let action = invoke_entry(entry, data);
        xdp_debug!("[xdp][ingress] executed: entry={} bytes={} action={}", entry, data.len(), action);
        action
    })
}

#[cfg(not(target_arch = "riscv32"))]
fn invoke_entry(_entry: usize, _data: &mut [u8]) -> i32 {
    crate::redirect::XDP_PASS
}

/// RV32IMC call into a leased AOT entry. The saved frame also remains the
/// tail-call return target used by `prog_table`.
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
        core::arch::asm!(
            "fence.i",
            "mv t0, sp",
            "lla t1, 1f",
            "sw t0, 0({frame})",
            "sw t1, 4({frame})",
            "sw s0, 8({frame})",
            "sw s1, 12({frame})",
            "sw s2, 16({frame})",
            "sw s3, 20({frame})",
            "sw s4, 24({frame})",
            "sw s5, 28({frame})",
            "sw s6, 32({frame})",
            "sw s7, 36({frame})",
            "sw s8, 40({frame})",
            "sw s9, 44({frame})",
            "sw s10, 48({frame})",
            "sw s11, 52({frame})",
            "jalr {entry}",
            "1:",
            frame = in(reg) frame,
            entry = in(reg) entry,
            in("a0") ctx_ptr,
            lateout("a0") result,
            out("t0") _,
            out("t1") _,
            clobber_abi("C"),
        );
    }
    ctx::record_window(&ctx);
    result
}
