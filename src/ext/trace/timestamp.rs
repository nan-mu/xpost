//! Helper enter/leave timestamps (`feature = "trace-timestamp"`).
//!
//! Clock: [`esp_hal::time::Instant`] µs since epoch (`u32`, wraps ~4294 s).
//! Attach target is the crate-mounted ingress program; this type is only the ring storage.

use core::sync::atomic::Ordering;

use portable_atomic::{AtomicU8, AtomicU32, AtomicUsize};

/// Ring capacity (fixed; overflow increments [`Timestamp::timestamp_dropped`]).
pub const CAP: usize = 128;

/// Event kind for the trace ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EventKind {
    HelperEnter = 1,
    HelperLeave = 2,
}

impl EventKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::HelperEnter),
            2 => Some(Self::HelperLeave),
            _ => None,
        }
    }
}

/// One ring entry (copyable snapshot).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub kind: u8,
    pub helper_id: u8,
    pub t_us: u32,
}

/// Helper timestamp ring.
pub struct Timestamp {
    kind: [AtomicU8; CAP],
    helper_id: [AtomicU8; CAP],
    t_us: [AtomicU32; CAP],
    n: AtomicUsize,
    dropped: AtomicUsize,
}

impl Timestamp {
    pub const fn new() -> Self {
        Self {
            kind: [const { AtomicU8::new(0) }; CAP],
            helper_id: [const { AtomicU8::new(0) }; CAP],
            t_us: [const { AtomicU32::new(0) }; CAP],
            n: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn now_us() -> u32 {
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros() as u32
    }

    #[inline]
    pub fn helper_enter(&self, id: usize) {
        self.push(EventKind::HelperEnter, id);
    }

    #[inline]
    pub fn helper_leave(&self, id: usize) {
        self.push(EventKind::HelperLeave, id);
    }

    #[inline]
    fn push(&self, kind: EventKind, helper_id: usize) {
        let slot = self.n.fetch_add(1, Ordering::Relaxed);
        if slot >= CAP {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.kind[slot].store(kind as u8, Ordering::Relaxed);
        self.helper_id[slot].store(helper_id as u8, Ordering::Relaxed);
        self.t_us[slot].store(Self::now_us(), Ordering::Relaxed);
    }

    pub fn snapshot_timestamps(&self, out: &mut [Event]) -> usize {
        let n = self.n.load(Ordering::Acquire).min(CAP);
        if n == 0 || out.is_empty() {
            return 0;
        }
        let take = n.min(out.len());
        for (i, slot) in out.iter_mut().enumerate().take(take) {
            *slot = Event {
                kind: self.kind[i].load(Ordering::Relaxed),
                helper_id: self.helper_id[i].load(Ordering::Relaxed),
                t_us: self.t_us[i].load(Ordering::Relaxed),
            };
        }
        take
    }

    pub fn clear_timestamps(&self) {
        self.n.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        for i in 0..CAP {
            self.kind[i].store(0, Ordering::Relaxed);
            self.helper_id[i].store(0, Ordering::Relaxed);
            self.t_us[i].store(0, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn timestamp_len(&self) -> usize {
        self.n.load(Ordering::Relaxed).min(CAP)
    }

    #[inline]
    pub fn timestamp_dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Helper enter on the mounted program's timestamp ring.
#[inline]
pub(crate) fn helper_enter(id: usize) {
    crate::xdp::ingress::XDP_PROG.timestamp.helper_enter(id);
}

/// Helper leave on the mounted program's timestamp ring.
#[inline]
pub(crate) fn helper_leave(id: usize) {
    crate::xdp::ingress::XDP_PROG.timestamp.helper_leave(id);
}
