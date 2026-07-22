//! XDP program slots in **DRAM** (`.bss`) — hot-update staging.
//!
//! Execution still uses IRAM [`crate::patch::part`] (`HOT_REGION`); this module
//! only stores images and wires map ABI. A future path will copy DRAM → IRAM
//! (or jump from a DRAM-executable region) — not implemented here.
//!
//! On [`install`]: image is copied into a free DRAM slot, then each
//! `(aot_addr, ResourceId)` bind is published via [`super::map::abi`] and the
//! map's `users` refcount is incremented. [`remove`] reverses that.
//!
//! Slot locking uses [`crate::patch::sync::RwCell`] (`patch-sync-embassy` /
//! `patch-sync-st`).

use core::ptr;

use crate::patch::sync::{self, RwCell};

use super::map::abi::{self, AliasError, MAX_MAP_ALIASES};
use super::map::pool::{RawResource, ResourceId};
use super::mem_map::HOT_REGION_LEN;

/// Concurrent staged programs (DRAM only).
pub const XDP_SLOT_COUNT: usize = 2;
/// Per-slot capacity — aligned with IRAM hot window size.
pub const XDP_SLOT_LEN: usize = HOT_REGION_LEN;
/// Max map binds recorded per program.
pub const XDP_MAX_BINDS: usize = MAX_MAP_ALIASES;

/// Handle for a staged XDP image (slot index).
pub type ProgramId = u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramError {
    TooLarge,
    NoSlot,
    MapMissing,
    NotFound,
    AliasFull,
    Busy,
}

impl From<AliasError> for ProgramError {
    fn from(_: AliasError) -> Self {
        Self::AliasFull
    }
}

/// Install command: raw AOT bytes + map ABI binds (`aot_addr` → pool Id).
pub struct XdpInstallCmd<'a> {
    pub image: &'a [u8],
    /// Linker VMAs of `.maps` objects inside the image (or absolute if pre-linked).
    pub map_binds: &'a [(usize, ResourceId)],
}

#[unsafe(link_section = ".bss")]
static mut XDP_DRAM: [[u8; XDP_SLOT_LEN]; XDP_SLOT_COUNT] = [[0; XDP_SLOT_LEN]; XDP_SLOT_COUNT];

struct SlotMeta {
    used: bool,
    len: usize,
    binds: heapless::Vec<(usize, ResourceId), XDP_MAX_BINDS>,
}

static SLOTS: RwCell<[SlotMeta; XDP_SLOT_COUNT]> = RwCell::new([
    SlotMeta {
        used: false,
        len: 0,
        binds: heapless::Vec::new(),
    },
    SlotMeta {
        used: false,
        len: 0,
        binds: heapless::Vec::new(),
    },
]);

fn try_begin() -> Result<(), ProgramError> {
    if sync::ctrl_try_begin() {
        Ok(())
    } else {
        Err(ProgramError::Busy)
    }
}

fn end() {
    sync::ctrl_end();
}

fn copy_image(slot_idx: usize, image: &[u8]) {
    unsafe {
        let dst = ptr::addr_of_mut!(XDP_DRAM[slot_idx]) as *mut u8;
        ptr::copy_nonoverlapping(image.as_ptr(), dst, image.len());
        if image.len() < XDP_SLOT_LEN {
            ptr::write_bytes(dst.add(image.len()), 0, XDP_SLOT_LEN - image.len());
        }
    }
}

fn clear_slot_dram(idx: usize) {
    unsafe {
        let dst = ptr::addr_of_mut!(XDP_DRAM[idx]) as *mut u8;
        ptr::write_bytes(dst, 0, XDP_SLOT_LEN);
    }
}

/// Stage an XDP image in DRAM and publish its map ABI.
///
/// Does **not** copy into IRAM / does **not** start execution.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn install(cmd: XdpInstallCmd<'_>) -> Result<ProgramId, ProgramError> {
    try_begin()?;
    let result = install_inner(cmd).await;
    end();
    result
}

#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
async fn install_inner(cmd: XdpInstallCmd<'_>) -> Result<ProgramId, ProgramError> {
    if cmd.image.len() > XDP_SLOT_LEN {
        return Err(ProgramError::TooLarge);
    }
    if cmd.map_binds.len() > XDP_MAX_BINDS {
        return Err(ProgramError::AliasFull);
    }

    for &(_, id) in cmd.map_binds {
        if !RawResource::contains(id).await {
            return Err(ProgramError::MapMissing);
        }
    }

    let mut guard = SLOTS.write().await;
    let slot_idx = guard.iter().position(|s| !s.used).ok_or(ProgramError::NoSlot)?;

    copy_image(slot_idx, cmd.image);

    let mut applied = 0usize;
    for &(addr, id) in cmd.map_binds {
        if !abi::register_map_addr(addr, id) {
            for &(a, prev_id) in &cmd.map_binds[..applied] {
                abi::unbind_map_addr(a);
                let _ = RawResource::remove_user(prev_id).await;
            }
            return Err(ProgramError::AliasFull);
        }
        if !RawResource::add_user(id).await {
            abi::unbind_map_addr(addr);
            for &(a, prev_id) in &cmd.map_binds[..applied] {
                abi::unbind_map_addr(a);
                let _ = RawResource::remove_user(prev_id).await;
            }
            return Err(ProgramError::MapMissing);
        }
        applied += 1;
    }

    let mut binds = heapless::Vec::new();
    for &b in cmd.map_binds {
        let _ = binds.push(b);
    }

    guard[slot_idx] = SlotMeta {
        used: true,
        len: cmd.image.len(),
        binds,
    };

    Ok(slot_idx as ProgramId)
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn install(cmd: XdpInstallCmd<'_>) -> Result<ProgramId, ProgramError> {
    try_begin()?;
    let result = install_inner(cmd);
    end();
    result
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
fn install_inner(cmd: XdpInstallCmd<'_>) -> Result<ProgramId, ProgramError> {
    if cmd.image.len() > XDP_SLOT_LEN {
        return Err(ProgramError::TooLarge);
    }
    if cmd.map_binds.len() > XDP_MAX_BINDS {
        return Err(ProgramError::AliasFull);
    }

    for &(_, id) in cmd.map_binds {
        if !RawResource::contains(id) {
            return Err(ProgramError::MapMissing);
        }
    }

    let mut guard = SLOTS.write();
    let slot_idx = guard.iter().position(|s| !s.used).ok_or(ProgramError::NoSlot)?;

    copy_image(slot_idx, cmd.image);

    let mut applied = 0usize;
    for &(addr, id) in cmd.map_binds {
        if !abi::register_map_addr(addr, id) {
            for &(a, prev_id) in &cmd.map_binds[..applied] {
                abi::unbind_map_addr(a);
                let _ = RawResource::remove_user(prev_id);
            }
            return Err(ProgramError::AliasFull);
        }
        if !RawResource::add_user(id) {
            abi::unbind_map_addr(addr);
            for &(a, prev_id) in &cmd.map_binds[..applied] {
                abi::unbind_map_addr(a);
                let _ = RawResource::remove_user(prev_id);
            }
            return Err(ProgramError::MapMissing);
        }
        applied += 1;
    }

    let mut binds = heapless::Vec::new();
    for &b in cmd.map_binds {
        let _ = binds.push(b);
    }

    guard[slot_idx] = SlotMeta {
        used: true,
        len: cmd.image.len(),
        binds,
    };

    Ok(slot_idx as ProgramId)
}

/// Remove a staged program: unbind ABI, drop map users, free DRAM slot.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn remove(id: ProgramId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = remove_inner(id).await;
    end();
    result
}

#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
async fn remove_inner(id: ProgramId) -> Result<(), ProgramError> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }

    let mut guard = SLOTS.write().await;
    if !guard[idx].used {
        return Err(ProgramError::NotFound);
    }

    let binds = core::mem::take(&mut guard[idx].binds);
    for &(addr, map_id) in binds.iter() {
        abi::unbind_map_addr(addr);
        let _ = RawResource::remove_user(map_id).await;
    }

    guard[idx] = SlotMeta {
        used: false,
        len: 0,
        binds: heapless::Vec::new(),
    };

    clear_slot_dram(idx);
    Ok(())
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn remove(id: ProgramId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = remove_inner(id);
    end();
    result
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
fn remove_inner(id: ProgramId) -> Result<(), ProgramError> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }

    let mut guard = SLOTS.write();
    if !guard[idx].used {
        return Err(ProgramError::NotFound);
    }

    let binds = core::mem::take(&mut guard[idx].binds);
    for &(addr, map_id) in binds.iter() {
        abi::unbind_map_addr(addr);
        let _ = RawResource::remove_user(map_id);
    }

    guard[idx] = SlotMeta {
        used: false,
        len: 0,
        binds: heapless::Vec::new(),
    };

    clear_slot_dram(idx);
    Ok(())
}

/// Borrow the staged DRAM image (for a future IRAM promote / inspect path).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn image(id: ProgramId) -> Option<&'static [u8]> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return None;
    }
    let guard = SLOTS.read().await;
    if !guard[idx].used {
        return None;
    }
    let len = guard[idx].len;
    // SAFETY: slot stays occupied while program id is valid; buffer is 'static.
    Some(unsafe { core::slice::from_raw_parts(ptr::addr_of!(XDP_DRAM[idx]) as *const u8, len) })
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn image(id: ProgramId) -> Option<&'static [u8]> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return None;
    }
    let guard = SLOTS.read();
    if !guard[idx].used {
        return None;
    }
    let len = guard[idx].len;
    Some(unsafe { core::slice::from_raw_parts(ptr::addr_of!(XDP_DRAM[idx]) as *const u8, len) })
}

/// Map binds recorded for a staged program.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn map_binds(id: ProgramId) -> Option<heapless::Vec<(usize, ResourceId), XDP_MAX_BINDS>> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return None;
    }
    let guard = SLOTS.read().await;
    if !guard[idx].used {
        return None;
    }
    Some(guard[idx].binds.clone())
}

#[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
pub fn map_binds(id: ProgramId) -> Option<heapless::Vec<(usize, ResourceId), XDP_MAX_BINDS>> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return None;
    }
    let guard = SLOTS.read();
    if !guard[idx].used {
        return None;
    }
    Some(guard[idx].binds.clone())
}
