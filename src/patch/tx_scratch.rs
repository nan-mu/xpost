//! Scheme A TX scratch buffer — independent of RX `PENDING_FRAME` egress queue.
//!
//! Single-flight: only one hijacked TX consume at a time.

use core::cell::RefCell;

use critical_section::Mutex;

/// Same order of magnitude as `redirect::PENDING_FRAME_CAP`.
pub const TX_SCRATCH_CAP: usize = 1600;

struct ScratchInner {
    buf: [u8; TX_SCRATCH_CAP],
    /// Bytes written by the stack into `buf` for this flight.
    len: usize,
    busy: bool,
}

static SCRATCH: Mutex<RefCell<ScratchInner>> = Mutex::new(RefCell::new(ScratchInner {
    buf: [0u8; TX_SCRATCH_CAP],
    len: 0,
    busy: false,
}));

/// Prepare scratch for `len` bytes. Returns false if busy or too large.
pub fn try_begin(len: usize) -> bool {
    if len == 0 || len > TX_SCRATCH_CAP {
        return false;
    }
    critical_section::with(|cs| {
        let mut s = SCRATCH.borrow_ref_mut(cs);
        if s.busy {
            return false;
        }
        s.busy = true;
        s.len = len;
        s.buf[..len].fill(0);
        true
    })
}

/// Mutable window for smoltcp to write the outbound frame.
///
/// # Safety
/// Caller must have succeeded [`try_begin`] and must not call concurrently.
pub fn frame_mut() -> &'static mut [u8] {
    critical_section::with(|cs| {
        let s = SCRATCH.borrow_ref(cs);
        let len = s.len;
        let ptr = s.buf.as_ptr() as *mut u8;
        unsafe { core::slice::from_raw_parts_mut(ptr, len) }
    })
}

/// Current flight length (stack frame size).
pub fn frame_len() -> usize {
    critical_section::with(|cs| SCRATCH.borrow_ref(cs).len)
}

/// End a flight (after TX / drop / fail-open).
pub fn end() {
    critical_section::with(|cs| {
        let mut s = SCRATCH.borrow_ref_mut(cs);
        s.busy = false;
        s.len = 0;
    });
}
