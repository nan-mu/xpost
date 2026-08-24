//! XDP program slots in **DRAM** (`.bss`) — hot-update staging.
//!
//! Execution uses IRAM [`crate::part`] (`HOT_A` / `HOT_B`). This module
//! stores images, wires map ABI, and [`promote`]s DRAM → IRAM.
//!
//! On [`install`]: image is copied into a free DRAM slot, then each
//! `(aot_addr, ResourceId)` bind is published via [`super::map::abi`] and the
//! map's `users` refcount is incremented. [`remove`] reverses that.
//!
//! Slot locking uses [`crate::sync::RwCell`] (`patch-sync-embassy` /
//! `patch-sync-st`).

use core::ptr;

use crate::sync::{self, RwCell};

use super::datapath;
use super::map::abi::{self, AliasError, MAX_MAP_ALIASES};
use super::map::pool::{RawResource, ResourceId};
use super::mem_map::{HOT_SLOT_COUNT, HOT_SLOT_LEN_MAX};
use super::mount::{self, MountError, MountKind};
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
    /// Named slot is not the current `rx` mount (切换).
    NotRx,
    /// Destination is not a `call` slot, or is already `rx` (热切换).
    NotCall,
    /// Layout has fewer than 2 hot slots.
    NeedTwoSlots,
    /// Empty payload but destination IRAM is not loaded, or 切换 with empty image.
    EmptySlot,
}

impl From<MountError> for ProgramError {
    fn from(e: MountError) -> Self {
        match e {
            MountError::Busy => Self::Busy,
            MountError::DuplicateRole | MountError::BadArg => Self::NotCall,
        }
    }
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

/// Stage raw bytes into a **fixed** DRAM slot (no map binds). Overwrites if occupied.
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

/// 切换: stop datapath, overwrite the current `rx` hot slot, re-enable.
///
/// `requested` must be the current `rx` slot, or `None` to use current `rx`.
/// `stash_dram` optionally copies the previous rx image into a DRAM staging slot.
pub async fn swap_rx(
    payload: &[u8],
    requested: Option<SlotId>,
    stash_dram: Option<ProgramId>,
) -> Result<(usize, usize), ProgramError> {
    try_begin()?;
    let result = swap_rx_inner(payload, requested, stash_dram).await;
    end();
    result
}

async fn swap_rx_inner(
    payload: &[u8],
    requested: Option<SlotId>,
    stash_dram: Option<ProgramId>,
) -> Result<(usize, usize), ProgramError> {
    let Some(rx) = mount::try_rx_slot() else {
        return Err(ProgramError::NotFound);
    };
    if let Some(req) = requested {
        if req != rx {
            return Err(ProgramError::NotRx);
        }
    }
    if payload.is_empty() {
        return Err(ProgramError::EmptySlot);
    }
    if payload.len() > rx.len() {
        return Err(ProgramError::TooLarge);
    }

    let was_active = datapath::is_active();
    if was_active {
        datapath::disable();
    }

    let stashed_len = if let Some(dram_id) = stash_dram {
        let idx = dram_id as usize;
        if idx >= XDP_SLOT_COUNT {
            if was_active {
                datapath::enable();
            }
            return Err(ProgramError::NotFound);
        }
        match part::read_slot(rx) {
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
                clear_dram_slot(idx).await;
                0
            }
        }
    } else {
        0
    };

    if part::write_slot(rx, payload).is_err() {
        if was_active {
            datapath::enable();
        }
        return Err(ProgramError::PromoteFailed);
    }

    if was_active {
        datapath::enable();
    }
    Ok((stashed_len, payload.len()))
}

/// 热切换: write `dst` (must be `call`) then flip the rx mount pointer. Datapath stays on.
pub async fn hot_swap_rx(dst: SlotId, payload: &[u8]) -> Result<usize, ProgramError> {
    try_begin()?;
    let result = hot_swap_rx_inner(dst, payload).await;
    end();
    result
}

async fn hot_swap_rx_inner(dst: SlotId, payload: &[u8]) -> Result<usize, ProgramError> {
    hot_swap_rx_prepare(dst, payload)?;
    mount::flip_rx(dst).await?;
    Ok(hot_swap_loaded_len(dst, payload))
}

fn hot_swap_rx_prepare(dst: SlotId, payload: &[u8]) -> Result<(), ProgramError> {
    if HOT_SLOT_COUNT < 2 {
        return Err(ProgramError::NeedTwoSlots);
    }
    let Some(cur_rx) = mount::try_rx_slot() else {
        return Err(ProgramError::NotFound);
    };
    if dst == cur_rx {
        return Err(ProgramError::NotCall);
    }
    match mount::try_mount(dst) {
        Some(MountKind::Call) => {}
        Some(MountKind::Rx) => return Err(ProgramError::NotCall),
        None => return Err(ProgramError::Busy),
    }
    if payload.is_empty() {
        if !prog_table::is_loaded(dst) {
            return Err(ProgramError::EmptySlot);
        }
        return Ok(());
    }
    if payload.len() > dst.len() {
        return Err(ProgramError::TooLarge);
    }
    if part::write_slot(dst, payload).is_err() {
        return Err(ProgramError::PromoteFailed);
    }
    Ok(())
}

fn hot_swap_loaded_len(dst: SlotId, payload: &[u8]) -> usize {
    if payload.is_empty() {
        prog_table::image_len(dst)
    } else {
        payload.len()
    }
}

/// Stage an XDP image in DRAM and publish its map ABI.
///
/// Does **not** copy into IRAM / does **not** start execution — call [`promote`].
pub async fn install(cmd: XdpInstallCmd<'_>) -> Result<ProgramId, ProgramError> {
    try_begin()?;
    let result = install_inner(cmd).await;
    end();
    result
}

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

/// Copy a staged DRAM image into an IRAM hot slot and publish prog-table entry.
///
/// Prefer [`SlotId::B`] for hot updates. Disables the datapath for the copy window.
pub async fn promote(id: ProgramId, slot: SlotId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = promote_inner(id, slot).await;
    end();
    result
}

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

/// Remove a staged program: unbind ABI, drop map users, free DRAM slot.
pub async fn remove(id: ProgramId) -> Result<(), ProgramError> {
    try_begin()?;
    let result = remove_inner(id).await;
    end();
    result
}

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

/// Borrow the staged DRAM image (for IRAM promote / inspect).
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

/// Map binds recorded for a staged program.
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
