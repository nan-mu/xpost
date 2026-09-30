//! Trace event rings (`feature = "perf-trace"`) — sole **timeline** substrate.
//!
//! Every path / flame stamp is a timestamped event pushed into a **categorized**
//! ring (one category → one buffer). Nest “spans” are not a second store: they
//! are paired events (`SpanBegin`/`SpanEnd`, `HelperEnter`/`HelperLeave`) on the
//! same ring; unpaired kinds are point milestones (`RxBegin`, `TxKick`, …).
//!
//! Rings today:
//! - **radio / XDP** ([`CAP`]): esp-radio + image helpers — flame path-rx/path-tx.
//! - **app** ([`APP_CAP`]): reserved wire slot; **no firmware writers** currently
//!   (`App*` kinds remain for ABI / future HAL stamps).
//!
//! High-pps formal metrics use Counter / Aggregate / Gauge — do not flood these
//! rings.

use core::sync::atomic::Ordering;

use heapless::Deque;
use portable_atomic::{AtomicU32, AtomicUsize};

/// Radio / XDP ring capacity (newest-wins overwrite).
pub const CAP: usize = 256;

/// App-category ring — reserved; writers removed until a HAL path returns.
pub const APP_CAP: usize = 64;

/// T-section wire size: hdr(16) + radio[CAP] + app[APP_CAP].
pub const WIRE_LEN: usize = 16 + CAP * 16 + APP_CAP * 16;

/// Predefined trace event kinds (wire `kind` byte).
///
/// All are ring events. Nesting for flames uses
/// [`Self::SpanBegin`]/[`Self::SpanEnd`] (`arg0` = [`xpost_type::ProfSite`]) and
/// [`Self::HelperEnter`]/[`Self::HelperLeave`] (`arg0` = BPF helper id).
///
/// `App*` (32–38): reserved for a future app-category writer; unused today.
/// `SlowExecution` / `UserMarker` / `RuntimeError` / `TxFlush`: reserved-legacy
/// (no live call-sites).
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
    SpanBegin = 9,
    SpanEnd = 10,
    HelperEnter = 11,
    HelperLeave = 12,
    RxDelivered = 13,
    RxDiscarded = 14,
    TxSubBegin = 15,
    TxSubmitted = 16,
    TxFailed = 17,
    TxCompleted = 18,
    TxFailBadBuf = 19,
    TxFailToken = 20,
    TxEnq = 21,
    TxKick = 22,
    TxPoll = 23,
    TxCapWait = 24,
    TxNomemWait = 25,
    TxBlock = 26,
    TxEnqFailClaim = 27,
    TxEnqFailRing = 28,
    TxFlush = 29,
    TxGrpSrc = 30,
    TxNoRetain = 31,
    /// Reserved app-category kinds (no writers; wire slots kept).
    AppUdpRecv = 32,
    AppEchoBegin = 33,
    AppEchoEnd = 34,
    AppUdpSendBegin = 35,
    AppUdpSendEnd = 36,
    AppWifiTxBegin = 37,
    AppWifiTxEnd = 38,
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
            9 => Some(Self::SpanBegin),
            10 => Some(Self::SpanEnd),
            11 => Some(Self::HelperEnter),
            12 => Some(Self::HelperLeave),
            13 => Some(Self::RxDelivered),
            14 => Some(Self::RxDiscarded),
            15 => Some(Self::TxSubBegin),
            16 => Some(Self::TxSubmitted),
            17 => Some(Self::TxFailed),
            18 => Some(Self::TxCompleted),
            19 => Some(Self::TxFailBadBuf),
            20 => Some(Self::TxFailToken),
            21 => Some(Self::TxEnq),
            22 => Some(Self::TxKick),
            23 => Some(Self::TxPoll),
            24 => Some(Self::TxCapWait),
            25 => Some(Self::TxNomemWait),
            26 => Some(Self::TxBlock),
            27 => Some(Self::TxEnqFailClaim),
            28 => Some(Self::TxEnqFailRing),
            29 => Some(Self::TxFlush),
            30 => Some(Self::TxGrpSrc),
            31 => Some(Self::TxNoRetain),
            32 => Some(Self::AppUdpRecv),
            33 => Some(Self::AppEchoBegin),
            34 => Some(Self::AppEchoEnd),
            35 => Some(Self::AppUdpSendBegin),
            36 => Some(Self::AppUdpSendEnd),
            37 => Some(Self::AppWifiTxBegin),
            38 => Some(Self::AppWifiTxEnd),
            _ => None,
        }
    }

    /// Stable name for host decode / `OK trace` lines.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RxBegin => "RxBegin",
            Self::XdpBegin => "XdpBegin",
            Self::XdpEnd => "XdpEnd",
            Self::RxEnd => "RxEnd",
            Self::SlowExecution => "SlowExecution",
            Self::RxOverflow => "RxOverflow",
            Self::RuntimeError => "RuntimeError",
            Self::UserMarker => "UserMarker",
            Self::SpanBegin => "SpanBegin",
            Self::SpanEnd => "SpanEnd",
            Self::HelperEnter => "HelperEnter",
            Self::HelperLeave => "HelperLeave",
            Self::RxDelivered => "RxDelivered",
            Self::RxDiscarded => "RxDiscarded",
            Self::TxSubBegin => "TxSubBegin",
            Self::TxSubmitted => "TxSubmitted",
            Self::TxFailed => "TxFailed",
            Self::TxCompleted => "TxCompleted",
            Self::TxFailBadBuf => "TxFailBadBuf",
            Self::TxFailToken => "TxFailToken",
            Self::TxEnq => "TxEnq",
            Self::TxKick => "TxKick",
            Self::TxPoll => "TxPoll",
            Self::TxCapWait => "TxCapWait",
            Self::TxNomemWait => "TxNomemWait",
            Self::TxBlock => "TxBlock",
            Self::TxEnqFailClaim => "TxEnqFailClaim",
            Self::TxEnqFailRing => "TxEnqFailRing",
            Self::TxFlush => "TxFlush",
            Self::TxGrpSrc => "TxGrpSrc",
            Self::TxNoRetain => "TxNoRetain",
            Self::AppUdpRecv => "AppUdpRecv",
            Self::AppEchoBegin => "AppEchoBegin",
            Self::AppEchoEnd => "AppEchoEnd",
            Self::AppUdpSendBegin => "AppUdpSendBegin",
            Self::AppUdpSendEnd => "AppUdpSendEnd",
            Self::AppWifiTxBegin => "AppWifiTxBegin",
            Self::AppWifiTxEnd => "AppWifiTxEnd",
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

/// Two categorized rings, one clock: radio/XDP (live) + app (reserved empty).
pub struct Trace {
    radio: critical_section::Mutex<core::cell::RefCell<Deque<Event, CAP>>>,
    app: critical_section::Mutex<core::cell::RefCell<Deque<Event, APP_CAP>>>,
    radio_len: AtomicUsize,
    app_len: AtomicUsize,
    radio_dropped: AtomicUsize,
    app_dropped: AtomicUsize,
    slow_threshold_us: AtomicU32,
}

impl Trace {
    pub const fn new() -> Self {
        Self {
            radio: critical_section::Mutex::new(core::cell::RefCell::new(Deque::new())),
            app: critical_section::Mutex::new(core::cell::RefCell::new(Deque::new())),
            radio_len: AtomicUsize::new(0),
            app_len: AtomicUsize::new(0),
            radio_dropped: AtomicUsize::new(0),
            app_dropped: AtomicUsize::new(0),
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
    fn push_ring<const N: usize>(
        q_mtx: &critical_section::Mutex<core::cell::RefCell<Deque<Event, N>>>,
        len: &AtomicUsize,
        dropped: &AtomicUsize,
        kind: EventKind,
        arg0: u32,
        arg1: u32,
    ) {
        let ev = Event {
            kind: kind as u8,
            arg0,
            arg1,
            t_us: Self::now_us(),
        };
        let n = critical_section::with(|cs| {
            let mut q = q_mtx.borrow_ref_mut(cs);
            if q.is_full() {
                let _ = q.pop_front();
                dropped.fetch_add(1, Ordering::Relaxed);
            }
            let _ = q.push_back(ev);
            q.len()
        });
        len.store(n, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn push(&self, kind: EventKind, arg0: u32, arg1: u32) {
        Self::push_ring(&self.radio, &self.radio_len, &self.radio_dropped, kind, arg0, arg1);
        #[cfg(feature = "perf-gauge")]
        crate::xdp::ingress::XDP_PROG
            .gauge
            .trace_buffer_usage
            .set(self.radio_len.load(Ordering::Relaxed).min(CAP));
    }

    #[inline]
    #[allow(dead_code)] // used when perf-aggregate + perf-trace
    pub(crate) fn maybe_slow(&self, dur_us: u32, action: u32) {
        // Threshold defaults to u32::MAX (off); no public setter — reserved SlowExecution.
        let thr = self.slow_threshold_us.load(Ordering::Relaxed);
        if thr != u32::MAX && dur_us >= thr {
            self.push(EventKind::SlowExecution, dur_us, action);
        }
    }

    pub fn snapshot_radio(&self, out: &mut [Event]) -> usize {
        critical_section::with(|cs| {
            let q = self.radio.borrow_ref(cs);
            let n = q.len().min(out.len());
            for (i, slot) in out.iter_mut().enumerate().take(n) {
                *slot = q.iter().nth(i).copied().unwrap_or_default();
            }
            n
        })
    }

    pub fn snapshot_app(&self, out: &mut [Event]) -> usize {
        critical_section::with(|cs| {
            let q = self.app.borrow_ref(cs);
            let n = q.len().min(out.len());
            for (i, slot) in out.iter_mut().enumerate().take(n) {
                *slot = q.iter().nth(i).copied().unwrap_or_default();
            }
            n
        })
    }

    pub fn clear(&self) {
        critical_section::with(|cs| {
            self.radio.borrow_ref_mut(cs).clear();
            self.app.borrow_ref_mut(cs).clear();
        });
        self.radio_len.store(0, Ordering::Relaxed);
        self.app_len.store(0, Ordering::Relaxed);
        self.radio_dropped.store(0, Ordering::Relaxed);
        self.app_dropped.store(0, Ordering::Relaxed);
        #[cfg(feature = "perf-gauge")]
        crate::xdp::ingress::XDP_PROG.gauge.trace_buffer_usage.set(0);
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.radio_len.load(Ordering::Relaxed).min(CAP)
    }

    #[inline]
    pub fn app_len(&self) -> usize {
        self.app_len.load(Ordering::Relaxed).min(APP_CAP)
    }

    #[inline]
    pub fn dropped(&self) -> usize {
        self.radio_dropped.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn app_dropped(&self) -> usize {
        self.app_dropped.load(Ordering::Relaxed)
    }
}
