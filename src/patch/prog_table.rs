//! IRAM hot-slot metadata + prog-array style index → entry table.
//!
//! Slot bases come from [`crate::patch::mem_map`]. Control plane updates entries;
//! [`bpf_tail_call`](super::helper) looks them up and may transfer without returning.

use core::cell::Cell;
use core::sync::atomic::{AtomicU8, Ordering};

use critical_section::Mutex;

use super::mem_map::{HOT_A_BASE, HOT_A_LEN, HOT_B_BASE, HOT_B_LEN};

/// Max nested `bpf_tail_call` transfers per RX invocation (eBPF uses 33; keep small on MCU).
pub const TAIL_CALL_MAX_DEPTH: u8 = 8;

/// Prog table capacity (indices for `bpf_tail_call`).
pub const PROG_TABLE_LEN: usize = 4;

/// IRAM hot-load slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SlotId {
    A = 0,
    B = 1,
}

impl SlotId {
    pub const fn base(self) -> usize {
        match self {
            Self::A => HOT_A_BASE,
            Self::B => HOT_B_BASE,
        }
    }

    pub const fn len(self) -> usize {
        match self {
            Self::A => HOT_A_LEN,
            Self::B => HOT_B_LEN,
        }
    }

    /// Entry = slot base (linker `ENTRY(_start)` at region origin).
    pub const fn entry(self) -> usize {
        self.base()
    }
}

#[derive(Clone, Copy)]
struct SlotState {
    loaded: bool,
    /// Valid image length inside the IRAM window (0 if empty).
    len: usize,
}

#[derive(Clone, Copy)]
struct TableInner {
    slots: [SlotState; 2],
    /// Index → absolute entry; 0 = empty.
    entries: [usize; PROG_TABLE_LEN],
}

static TABLE: Mutex<Cell<TableInner>> = Mutex::new(Cell::new(TableInner {
    slots: [
        SlotState {
            loaded: false,
            len: 0,
        },
        SlotState {
            loaded: false,
            len: 0,
        },
    ],
    entries: [0; PROG_TABLE_LEN],
}));

static TAIL_DEPTH: AtomicU8 = AtomicU8::new(0);

/// Saved by [`super::part::xdp`] before `jalr` into HOT_A — used by successful tail_call.
#[repr(C)]
pub struct XdpCallFrame {
    pub sp: usize,
    pub ra: usize,
}

pub static mut XDP_FRAME: XdpCallFrame = XdpCallFrame { sp: 0, ra: 0 };

fn with_table<R>(f: impl FnOnce(&mut TableInner) -> R) -> R {
    critical_section::with(|cs| {
        let cell = TABLE.borrow(cs);
        let mut inner = cell.get();
        let r = f(&mut inner);
        cell.set(inner);
        r
    })
}

/// Mark IRAM slot as containing a valid image of `len` bytes (`len=0` clears).
pub fn mark_loaded(slot: SlotId, loaded: bool) {
    mark_loaded_len(slot, if loaded { slot.len() } else { 0 });
}

/// Mark IRAM slot loaded with an exact image length (clamped to slot capacity).
pub fn mark_loaded_len(slot: SlotId, len: usize) {
    let len = core::cmp::min(len, slot.len());
    with_table(|t| {
        let s = &mut t.slots[slot as usize];
        if len == 0 {
            s.loaded = false;
            s.len = 0;
        } else {
            s.loaded = true;
            s.len = len;
        }
    });
}

pub fn is_loaded(slot: SlotId) -> bool {
    with_table(|t| t.slots[slot as usize].loaded)
}

pub fn image_len(slot: SlotId) -> usize {
    with_table(|t| {
        let s = t.slots[slot as usize];
        if s.loaded { s.len } else { 0 }
    })
}

/// Publish `entry` at prog-table `index` (0 clears).
pub fn set(index: usize, entry: Option<usize>) -> bool {
    if index >= PROG_TABLE_LEN {
        return false;
    }
    with_table(|t| {
        t.entries[index] = entry.unwrap_or(0);
    });
    true
}

pub fn get(index: usize) -> Option<usize> {
    if index >= PROG_TABLE_LEN {
        return None;
    }
    with_table(|t| {
        let e = t.entries[index];
        if e == 0 { None } else { Some(e) }
    })
}

pub fn clear(index: usize) {
    let _ = set(index, None);
}

/// Bind table index to a loaded slot's entry address.
pub fn set_slot(index: usize, slot: SlotId) -> bool {
    if !is_loaded(slot) {
        return false;
    }
    set(index, Some(slot.entry()))
}

pub fn reset_tail_depth() {
    TAIL_DEPTH.store(0, Ordering::Relaxed);
}

pub fn tail_depth() -> u8 {
    TAIL_DEPTH.load(Ordering::Relaxed)
}

/// Bump depth; returns `false` if the call would exceed [`TAIL_CALL_MAX_DEPTH`].
pub fn bump_tail_depth() -> bool {
    let depth = TAIL_DEPTH.load(Ordering::Relaxed);
    if depth >= TAIL_CALL_MAX_DEPTH {
        return false;
    }
    TAIL_DEPTH.store(depth + 1, Ordering::Relaxed);
    true
}

/// Transfer to `entry` with `ctx` in `a0`, restoring the [`part::xdp`](super::part::xdp) frame.
///
/// # Safety
/// `entry` must be a valid XDP entry in IRAM; `XDP_FRAME` must have been set by `xdp()`.
#[inline(never)]
pub unsafe fn transfer_to_entry(ctx: usize, entry: usize) -> ! {
    let sp = unsafe { XDP_FRAME.sp };
    let ra = unsafe { XDP_FRAME.ra };
    unsafe {
        core::arch::asm!(
            "mv sp, {sp}",
            "mv a0, {ctx}",
            "mv ra, {ra}",
            "jr {entry}",
            sp = in(reg) sp,
            ctx = in(reg) ctx,
            ra = in(reg) ra,
            entry = in(reg) entry,
            options(noreturn),
        );
    }
}
