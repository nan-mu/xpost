//! IRAM hot-slot metadata + prog-array style index → entry table.
//!
//! Slot bases come from [`crate::patch::mem_map`]. Control plane updates entries;
//! [`bpf_tail_call`](super::helper) looks them up and may transfer without returning.

use core::cell::Cell;
use core::sync::atomic::{AtomicU8, Ordering};

use critical_section::Mutex;

use super::mem_map::{HOT_SLOT_BASES, HOT_SLOT_COUNT, HOT_SLOT_LENS};

/// Max nested `bpf_tail_call` transfers per RX invocation (eBPF uses 33; keep small on MCU).
pub const TAIL_CALL_MAX_DEPTH: u8 = 8;

/// Prog table capacity (indices for `bpf_tail_call`).
pub const PROG_TABLE_LEN: usize = 4;

/// IRAM hot-load slot index (`0..HOT_SLOT_COUNT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct SlotId(pub u8);

impl SlotId {
    /// RX default entry (slot 0).
    pub const A: Self = Self(0);
    /// First hot-replace / tail_call target when `HOT_SLOT_COUNT >= 2`.
    pub const B: Self = Self(1);

    pub fn from_u8(v: u8) -> Option<Self> {
        if (v as usize) < HOT_SLOT_COUNT {
            Some(Self(v))
        } else {
            None
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub fn base(self) -> usize {
        HOT_SLOT_BASES[self.index()]
    }

    pub fn len(self) -> usize {
        HOT_SLOT_LENS[self.index()]
    }

    /// Entry = slot base (linker `ENTRY(_start)` at region origin).
    pub fn entry(self) -> usize {
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
    slots: [SlotState; HOT_SLOT_COUNT],
    /// Index → absolute entry; 0 = empty.
    entries: [usize; PROG_TABLE_LEN],
}

const EMPTY_SLOT: SlotState = SlotState {
    loaded: false,
    len: 0,
};

static TABLE: Mutex<Cell<TableInner>> = Mutex::new(Cell::new(TableInner {
    slots: [EMPTY_SLOT; HOT_SLOT_COUNT],
    entries: [0; PROG_TABLE_LEN],
}));

static TAIL_DEPTH: AtomicU8 = AtomicU8::new(0);

/// Saved by [`super::part::xdp`] before `jalr` into HOT slot 0 — used by successful tail_call.
///
/// Must include RV32 callee-saved regs (`s0`–`s11`): `bpf_tail_call` restores SP/RA and
/// jumps to another program without running the helper/A epilogue, so those saves on the
/// discarded stack would otherwise be lost (classic FP=`s0` → fault at `fp+off`).
#[repr(C)]
pub struct XdpCallFrame {
    pub sp: usize,
    pub ra: usize,
    pub s0: usize,
    pub s1: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
    pub s9: usize,
    pub s10: usize,
    pub s11: usize,
}

pub static mut XDP_FRAME: XdpCallFrame = XdpCallFrame {
    sp: 0,
    ra: 0,
    s0: 0,
    s1: 0,
    s2: 0,
    s3: 0,
    s4: 0,
    s5: 0,
    s6: 0,
    s7: 0,
    s8: 0,
    s9: 0,
    s10: 0,
    s11: 0,
};

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
        let s = &mut t.slots[slot.index()];
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
    with_table(|t| t.slots[slot.index()].loaded)
}

pub fn image_len(slot: SlotId) -> usize {
    with_table(|t| {
        let s = t.slots[slot.index()];
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

/// Clear every prog-table entry that points at `slot`'s entry address.
pub fn clear_entries_for_slot(slot: SlotId) {
    let entry = slot.entry();
    with_table(|t| {
        for e in t.entries.iter_mut() {
            if *e == entry {
                *e = 0;
            }
        }
    });
}

/// Unload IRAM slot and drop any prog-table entries that targeted it.
pub fn unload_slot(slot: SlotId) {
    clear_entries_for_slot(slot);
    mark_loaded_len(slot, 0);
}

/// Bind table index to a loaded slot's entry address.
pub fn set_slot(index: usize, slot: SlotId) -> bool {
    if !is_loaded(slot) {
        return false;
    }
    set(index, Some(slot.entry()))
}

/// After writing a non-entry hot slot: publish as `prog_table[0]` or clear if unloaded.
///
/// Slot 0 is the RX entry — writing it does not change the tail-call target.
pub fn publish_tail_callee(slot: SlotId) {
    if slot.index() == 0 {
        return;
    }
    if is_loaded(slot) {
        let _ = set_slot(0, slot);
    } else {
        clear_entries_for_slot(slot);
    }
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
/// Restores SP, RA, and callee-saved `s0`–`s11` saved before the HOT_A `jalr`, then
/// `jr entry`. The callee must return to that RA (same epilogue as slot-0 entry).
///
/// # Safety
/// `entry` must be a valid XDP entry in IRAM; `XDP_FRAME` must have been set by `xdp()`.
#[inline(never)]
pub unsafe fn transfer_to_entry(ctx: usize, entry: usize) -> ! {
    let frame = core::ptr::addr_of!(XDP_FRAME);
    unsafe {
        core::arch::asm!(
            "lw sp, 0({frame})",
            "lw ra, 4({frame})",
            "lw s0, 8({frame})",
            "lw s1, 12({frame})",
            "lw s2, 16({frame})",
            "lw s3, 20({frame})",
            "lw s4, 24({frame})",
            "lw s5, 28({frame})",
            "lw s6, 32({frame})",
            "lw s7, 36({frame})",
            "lw s8, 40({frame})",
            "lw s9, 44({frame})",
            "lw s10, 48({frame})",
            "lw s11, 52({frame})",
            "mv a0, {ctx}",
            "jr {entry}",
            frame = in(reg) frame,
            ctx = in(reg) ctx,
            entry = in(reg) entry,
            options(noreturn),
        );
    }
}
