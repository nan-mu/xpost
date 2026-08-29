//! RX scratch with leading headroom so AOT can `adjust_head(+N)` (peel headers).
//!
//! Layout: `[headroom | frame…]`. `XdpMd.data` points at the frame start.
//! Ingest is in-place for zero-copy TX; scratch stays for `adjust_tail` grow.

#![allow(dead_code)]

use core::cell::RefCell;

use critical_section::Mutex;

use super::ctx::XdpMd;

/// Bytes reserved before the frame for potential head expansion (unused today;
/// peel uses positive `adjust_head` from the frame start).
pub const RX_HEADROOM: usize = 128;
/// Max Ethernet frame we copy into scratch.
pub const RX_FRAME_CAP: usize = 1600;
const RX_SCRATCH_CAP: usize = RX_HEADROOM + RX_FRAME_CAP;

struct ScratchInner {
    buf: [u8; RX_SCRATCH_CAP],
    /// Absolute offset of current `data` within `buf` (starts at [`RX_HEADROOM`]).
    data_off: usize,
    /// Absolute offset of `data_end` within `buf`.
    end_off: usize,
    busy: bool,
}

static SCRATCH: Mutex<RefCell<ScratchInner>> = Mutex::new(RefCell::new(ScratchInner {
    buf: [0u8; RX_SCRATCH_CAP],
    data_off: RX_HEADROOM,
    end_off: RX_HEADROOM,
    busy: false,
}));

/// Copy `frame` into scratch after headroom. Returns false if busy / oversized.
pub fn try_begin(frame: &[u8]) -> bool {
    if frame.is_empty() || frame.len() > RX_FRAME_CAP {
        return false;
    }
    critical_section::with(|cs| {
        let mut s = SCRATCH.borrow_ref_mut(cs);
        if s.busy {
            return false;
        }
        s.busy = true;
        s.data_off = RX_HEADROOM;
        s.end_off = RX_HEADROOM + frame.len();
        let end = s.end_off;
        s.buf[RX_HEADROOM..end].copy_from_slice(frame);
        true
    })
}

/// Build `XdpMd` over the current flight window.
///
/// # Safety
/// Caller must have succeeded [`try_begin`] and hold the flight until [`end`].
pub fn xdp_md() -> XdpMd {
    critical_section::with(|cs| {
        let s = SCRATCH.borrow_ref(cs);
        let base = s.buf.as_ptr() as *mut u8;
        unsafe {
            XdpMd {
                data: base.add(s.data_off),
                data_end: base.add(s.end_off),
                data_meta: 0,
                ingress_ifindex: super::ctx::IFINDEX_WIFI,
                rx_queue_index: 0,
                egress_ifindex: 0,
            }
        }
    })
}

/// True if `addr` lies in the current RX scratch buffer (inclusive end).
pub fn ptr_in_window(addr: usize) -> bool {
    critical_section::with(|cs| {
        let s = SCRATCH.borrow_ref(cs);
        if !s.busy {
            return false;
        }
        let base = s.buf.as_ptr() as usize;
        addr >= base && addr <= base + RX_SCRATCH_CAP
    })
}

/// Sync scratch offsets from an AOT-mutated [`XdpMd`] (after `adjust_head`).
pub fn sync_from_md(md: &XdpMd) {
    critical_section::with(|cs| {
        let mut s = SCRATCH.borrow_ref_mut(cs);
        let base = s.buf.as_ptr() as usize;
        let data = md.data as usize;
        let end = md.data_end as usize;
        if data >= base && end >= data && end <= base + RX_SCRATCH_CAP {
            s.data_off = data - base;
            s.end_off = end - base;
        }
    })
}

/// Full frame as currently stored (from original frame start, not peeled).
pub fn frame_bytes() -> &'static [u8] {
    critical_section::with(|cs| {
        let s = SCRATCH.borrow_ref(cs);
        let len = s.end_off.saturating_sub(RX_HEADROOM);
        let ptr = unsafe { s.buf.as_ptr().add(RX_HEADROOM) };
        unsafe { core::slice::from_raw_parts(ptr, len.min(RX_FRAME_CAP)) }
    })
}

pub fn end() {
    critical_section::with(|cs| {
        let mut s = SCRATCH.borrow_ref_mut(cs);
        s.busy = false;
        s.data_off = RX_HEADROOM;
        s.end_off = RX_HEADROOM;
    });
}
