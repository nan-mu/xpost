//! XDP runtime ownership and execution state.
//!
//! This module is the only owner of XDP slot registration and the RX execution
//! snapshot. No other module owns an XDP activity switch or executable entry.

mod device;

pub use device::{Device, ZcTx};

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::{Mutex, MutexGuard};
use embassy_sync::once_lock::OnceLock;

/// Maximum slots represented by the first XdpRt implementation.
const MAX_SLOTS: usize = 4;
const MAX_MAP_REFS: usize = 16;
const NO_SLOT: usize = usize::MAX;

/// Library-owned XDP runtime. Its lock is intentionally the underlying
/// `OnceLock<Mutex<_>>`, rather than a wrapper type.
pub static XDP_RT: OnceLock<Mutex<CriticalSectionRawMutex, XdpRt>> = OnceLock::new();

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
    MapNotFound,
    MapRefLimit,
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
    slots: heapless::Vec<SlotMeta, MAX_SLOTS>,
    primary: usize,
    leased: bool,
    map_refs: heapless::Vec<crate::map::MapIdx, MAX_MAP_REFS>,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PRIMARY_ENTRY: AtomicUsize = AtomicUsize::new(0);
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

impl XdpRt {
    /// Register the first slot and publish the runtime singleton.
    pub async fn init<const N: usize>(
        slot: &'static Slot<N>,
    ) -> Result<MutexGuard<'static, CriticalSectionRawMutex, XdpRt>, Error> {
        let mut rt = Self {
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
        let end = (ptr as usize).checked_add(N).ok_or(Error::OverlappingSlot)?;
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

    /// Copy a cold-boot image into the primary slot. Map binding and image
    /// metadata validation intentionally remain TODO for the MapRt phase.
    pub fn load_primary(&mut self, image: &[u8]) -> Result<(), Error> {
        if self.leased {
            return Err(Error::Busy);
        }
        if image.is_empty() {
            return Err(Error::EmptySlot);
        }
        let slot = self.slots.get_mut(self.primary).ok_or(Error::NotLoaded)?;
        if image.len() > slot.len {
            return Err(Error::ImageTooLarge);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(image.as_ptr(), slot.ptr, image.len());
            core::ptr::write_bytes(slot.ptr.add(image.len()), 0, slot.len - image.len());
            #[cfg(target_arch = "riscv32")]
            core::arch::asm!("fence.i");
        }
        slot.loaded_len = image.len();
        Ok(())
    }

    /// Enable the primary slot.
    ///
    /// Passing a map runtime binds the generated AOT aliases and holds every
    /// referenced map until [`Self::release`].  The reference is an ownership
    /// lease, not a second runtime or a public raw-resource API.
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
                // The ref is recorded only after the increment succeeds, so
                // rollback cannot release somebody else's existing lease.
                let _ = self.map_refs.push(id);
            }
        }
        PRIMARY_ENTRY.store(slot.ptr as usize, Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        self.leased = true;
        Ok(())
    }

    /// Prevent new executions and wait for an in-progress RX invocation.
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
    ACTIVE
        .load(Ordering::Acquire)
        .then(|| PRIMARY_ENTRY.load(Ordering::Acquire))
        .filter(|entry| *entry != 0)
}

/// Whether a primary slot is currently leased for RX execution.
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
        return None;
    }
    let result = f(entry);
    IN_FLIGHT.fetch_sub(1, Ordering::Release);
    Some(result)
}

/// Invoke the currently leased primary image for one RX frame.
///
/// This is intentionally crate-private: the public injection point is
/// [`Device`], so callers cannot bypass the token and DMA lifetime rules.
pub(crate) fn execute(data: &mut [u8]) -> i32 {
    with_primary(|entry| invoke_entry(entry, data)).unwrap_or(crate::redirect::XDP_PASS)
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
