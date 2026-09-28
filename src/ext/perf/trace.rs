//! Trace event ring (`feature = "perf-trace"`) — heapless fixed capacity.

use core::sync::atomic::Ordering;

use heapless::Deque;
use portable_atomic::{AtomicU32, AtomicUsize};

/// Ring capacity.
pub const CAP: usize = 128;

/// Predefined trace event kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EventKind {
    RxBegin = 1,
    XdpBegin = 2,
    XdpEnd = 3,
    RxEnd = 4,
    SlowExecution = 5,
    RxOverflow = 6,
    RuntimeError = 7,
    UserMarker = 8,
}

impl EventKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::RxBegin),
            2 => Some(Self::XdpBegin),
            3 => Some(Self::XdpEnd),
            4 => Some(Self::RxEnd),
            5 => Some(Self::SlowExecution),
            6 => Some(Self::RxOverflow),
            7 => Some(Self::RuntimeError),
            8 => Some(Self::UserMarker),
            _ => None,
        }
    }
}

/// One ring entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    pub kind: u8,
    pub arg0: u32,
    pub arg1: u32,
    pub t_us: u32,
}

/// Heapless trace ring (stop-on-full).
pub struct Trace {
    // Interior mutability via critical section for Deque.
    buf: critical_section::Mutex<core::cell::RefCell<Deque<Event, CAP>>>,
    len: AtomicUsize,
    dropped: AtomicUsize,
    slow_threshold_us: AtomicU32,
}

impl Trace {
    pub const fn new() -> Self {
        Self {
            buf: critical_section::Mutex::new(core::cell::RefCell::new(Deque::new())),
            len: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            slow_threshold_us: AtomicU32::new(u32::MAX),
        }
    }

    #[inline]
    fn now_us() -> u32 {
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros() as u32
    }

    #[inline]
    pub fn set_slow_threshold_us(&self, us: u32) {
        self.slow_threshold_us.store(us, Ordering::Relaxed);
    }

    #[inline]
    pub fn slow_threshold_us(&self) -> u32 {
        self.slow_threshold_us.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn push(&self, kind: EventKind, arg0: u32, arg1: u32) {
        let ev = Event {
            kind: kind as u8,
            arg0,
            arg1,
            t_us: Self::now_us(),
        };
        let ok = critical_section::with(|cs| {
            let mut q = self.buf.borrow_ref_mut(cs);
            q.push_back(ev).is_ok()
        });
        if ok {
            self.len.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "perf-gauge")]
            crate::xdp::ingress::XDP_PROG
                .gauge
                .trace_buffer_usage
                .set(self.len.load(Ordering::Relaxed).min(CAP));
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    pub(crate) fn maybe_slow(&self, dur_us: u32, action: u32) {
        let thr = self.slow_threshold_us();
        if thr != u32::MAX && dur_us >= thr {
            self.push(EventKind::SlowExecution, dur_us, action);
        }
    }

    /// User-facing marker.
    #[inline]
    pub fn user_marker(&self, value: u32) {
        self.push(EventKind::UserMarker, value, 0);
    }

    pub fn snapshot(&self, out: &mut [Event]) -> usize {
        critical_section::with(|cs| {
            let q = self.buf.borrow_ref(cs);
            let n = q.len().min(out.len());
            for (i, slot) in out.iter_mut().enumerate().take(n) {
                *slot = q.iter().nth(i).copied().unwrap_or_default();
            }
            n
        })
    }

    pub fn clear(&self) {
        critical_section::with(|cs| {
            self.buf.borrow_ref_mut(cs).clear();
        });
        self.len.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        #[cfg(feature = "perf-gauge")]
        crate::xdp::ingress::XDP_PROG.gauge.trace_buffer_usage.set(0);
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed).min(CAP)
    }

    #[inline]
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}
