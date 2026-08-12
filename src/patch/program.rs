//! XDP program slots in **DRAM** (`.bss`) — hot-update staging.
//!
//! Execution uses IRAM [`crate::patch::part`] (`HOT_A` / `HOT_B`). This module
//! stores images, wires map ABI, and [`promote`]s DRAM → IRAM.
//!
//! On [`install`]: image is copied into a free DRAM slot, then each
//! `(aot_addr, ResourceId)` bind is published via [`super::map::abi`] and the
//! map's `users` refcount is incremented. [`remove`] reverses that.
//!
//! Slot locking uses [`crate::patch::sync::RwCell`] (`patch-sync-embassy` /
//! `patch-sync-st`).

use core::ptr;

use crate::patch::sync::{self, RwCell};

use super::datapath;
use super::map::abi::{self, AliasError, MAX_MAP_ALIASES};
use super::map::pool::{RawResource, ResourceId};
use super::mem_map::HOT_SLOT_LEN_MAX;
use super::part;
use super::prog_table::{self, SlotId};

/// Concurrent staged programs (DRAM only).
pub const XDP_SLOT_COUNT: usize = 2;
/// Per-slot capacity — sized for the largest IRAM hot window.
pub const XDP_SLOT_LEN: usize = HOT_SLOT_LEN_MAX;
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
    /// Target IRAM slot rejected the image (size / write failure).
    PromoteFailed,
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

fn do_promote(idx: usize, len: usize, slot: SlotId) -> Result<(), ProgramError> {
    if len > slot.len() {
        return Err(ProgramError::TooLarge);
    }
    let was_active = datapath::is_active();
    if was_active {
        datapath::disable();
    }
    let image =
        unsafe { core::slice::from_raw_parts(ptr::addr_of!(XDP_DRAM[idx]) as *const u8, len) };
    if part::write_slot(slot, image).is_err() {
        if was_active {
            datapath::enable();
        }
        return Err(ProgramError::PromoteFailed);
    }
    if slot.index() != 0 {
        let _ = prog_table::set_slot(0, slot);
    }
    if was_active {
        datapath::enable();
    }
    Ok(())
}

/// Clear DRAM slot metadata + image (unbind maps if present).
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
async fn clear_dram_slot(idx: usize) {
    let mut guard = SLOTS.write().await;
    if !guard[idx].used {
        return;
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
}

#[cfg(feature = "patch-sync-st")]
fn clear_dram_slot(idx: usize) {
    let mut guard = SLOTS.write();
    if !guard[idx].used {
        return;
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
}

/// Stage raw bytes into a **fixed** DRAM slot (no map binds). Overwrites if occupied.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn stage_raw_into(idx: usize, image: &[u8]) -> Result<(), ProgramError> {
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }
    if image.len() > XDP_SLOT_LEN {
        return Err(ProgramError::TooLarge);
    }
    clear_dram_slot(idx).await;
    copy_image(idx, image);
    let mut guard = SLOTS.write().await;
    guard[idx] = SlotMeta {
        used: true,
        len: image.len(),
        binds: heapless::Vec::new(),
    };
    Ok(())
}

#[cfg(feature = "patch-sync-st")]
pub fn stage_raw_into(idx: usize, image: &[u8]) -> Result<(), ProgramError> {
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }
    if image.len() > XDP_SLOT_LEN {
        return Err(ProgramError::TooLarge);
    }
    clear_dram_slot(idx);
    copy_image(idx, image);
    let mut guard = SLOTS.write();
    guard[idx] = SlotMeta {
        used: true,
        len: image.len(),
        binds: heapless::Vec::new(),
    };
    Ok(())
}

/// Hot-update swap: stash current IRAM `hot` into DRAM `dram_id`, then write `payload` into `hot`.
///
/// Typical test path: `dram_id=1`（一号槽）, `hot=SlotId::B`.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn swap_hot(
    dram_id: ProgramId,
    hot: SlotId,
    payload: &[u8],
) -> Result<(usize, usize), ProgramError> {
    try_begin()?;
    let result = swap_hot_inner(dram_id, hot, payload).await;
    end();
    result
}

#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
async fn swap_hot_inner(
    dram_id: ProgramId,
    hot: SlotId,
    payload: &[u8],
) -> Result<(usize, usize), ProgramError> {
    let idx = dram_id as usize;
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }
    if payload.len() > hot.len() {
        return Err(ProgramError::TooLarge);
    }

    let was_active = datapath::is_active();
    if was_active {
        datapath::disable();
    }

    let stashed_len = match part::read_slot(hot) {
        Some(cur) => {
            if let Err(e) = stage_raw_into(idx, cur).await {
                if was_active {
                    datapath::enable();
                }
                return Err(e);
            }
            cur.len()
        }
        None => {
            // Empty hot: still clear DRAM slot so tests see a known state.
            clear_dram_slot(idx).await;
            0
        }
    };

    if part::write_slot(hot, payload).is_err() {
        if was_active {
            datapath::enable();
        }
        return Err(ProgramError::PromoteFailed);
    }
    if hot.index() != 0 {
        let _ = prog_table::set_slot(0, hot);
    }

    if was_active {
        datapath::enable();
    }
    Ok((stashed_len, payload.len()))
}

#[cfg(feature = "patch-sync-st")]
pub fn swap_hot(
    dram_id: ProgramId,
    hot: SlotId,
    payload: &[u8],
) -> Result<(usize, usize), ProgramError> {
    try_begin()?;
    let result = swap_hot_inner(dram_id, hot, payload);
    end();
    result
}

#[cfg(feature = "patch-sync-st")]
fn swap_hot_inner(
    dram_id: ProgramId,
    hot: SlotId,
    payload: &[u8],
) -> Result<(usize, usize), ProgramError> {
    let idx = dram_id as usize;
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }
    if payload.len() > hot.len() {
        return Err(ProgramError::TooLarge);
    }

    let was_active = datapath::is_active();
    if was_active {
        datapath::disable();
    }

    let stashed_len = match part::read_slot(hot) {
        Some(cur) => {
            if let Err(e) = stage_raw_into(idx, cur) {
                if was_active {
                    datapath::enable();
                }
                return Err(e);
            }
            cur.len()
        }
        None => {
            clear_dram_slot(idx);
            0
        }
    };

    if part::write_slot(hot, payload).is_err() {
        if was_active {
            datapath::enable();
        }
        return Err(ProgramError::PromoteFailed);
    }
    if hot.index() != 0 {
        let _ = prog_table::set_slot(0, hot);
    }

    if was_active {
        datapath::enable();
    }
    Ok((stashed_len, payload.len()))
}

/// Stage an XDP image in DRAM and publish its map ABI.
///
/// Does **not** copy into IRAM / does **not** start execution — call [`promote`].
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
    let slot_idx = guard
        .iter()
        .position(|s| !s.used)
        .ok_or(ProgramError::NoSlot)?;

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

#[cfg(feature = "patch-sync-st")]
pub fn install(cmd: XdpInstallCmd<'_>) -> Result<ProgramId, ProgramError> {
    try_begin()?;
    let result = install_inner(cmd);
    end();
    result
}

#[cfg(feature = "patch-sync-st")]
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
    let slot_idx = guard
        .iter()
        .position(|s| !s.used)
        .ok_or(ProgramError::NoSlot)?;

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

/// Copy a staged DRAM image into an IRAM hot slot and publish prog-table entry.
///
/// Prefer [`SlotId::B`] for hot updates. Disables the datapath for the copy window.
#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
pub async fn promote(id: ProgramId, slot: SlotId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = promote_inner(id, slot).await;
    end();
    result
}

#[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
async fn promote_inner(id: ProgramId, slot: SlotId) -> Result<(), ProgramError> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }
    let len = {
        let guard = SLOTS.read().await;
        if !guard[idx].used {
            return Err(ProgramError::NotFound);
        }
        guard[idx].len
    };
    do_promote(idx, len, slot)
}

#[cfg(feature = "patch-sync-st")]
pub fn promote(id: ProgramId, slot: SlotId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = promote_inner(id, slot);
    end();
    result
}

#[cfg(feature = "patch-sync-st")]
fn promote_inner(id: ProgramId, slot: SlotId) -> Result<(), ProgramError> {
    let idx = id as usize;
    if idx >= XDP_SLOT_COUNT {
        return Err(ProgramError::NotFound);
    }
    let len = {
        let guard = SLOTS.read();
        if !guard[idx].used {
            return Err(ProgramError::NotFound);
        }
        guard[idx].len
    };
    do_promote(idx, len, slot)
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

#[cfg(feature = "patch-sync-st")]
pub fn remove(id: ProgramId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = remove_inner(id);
    end();
    result
}

#[cfg(feature = "patch-sync-st")]
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

/// Borrow the staged DRAM image (for IRAM promote / inspect).
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

#[cfg(feature = "patch-sync-st")]
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

#[cfg(feature = "patch-sync-st")]
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
