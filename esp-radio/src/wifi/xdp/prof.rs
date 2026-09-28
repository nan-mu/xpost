//! Heapless CPU / span profiling for native-XDP paths.
//!
//! Two complementary views, both static and dumpable over serial:
//!
//! 1. **Spans** — named `mcycle` accumulators (`xdp_run`, `tx_enqueue`, …).
//!    Dump as Brendan Gregg folded stacks for speedscope / `flamegraph.pl`:
//!    `wifi_rx;xdp_run 12345`
//!
//! 2. **PC hits** — open-addressed histogram of raw program counters.
//!    Host resolves with `llvm-addr2line -e <firmware.elf> 0x…` then folds.
//!
//! No allocator. Safe to call from RX / TX-done contexts (atomics only).

#![allow(missing_docs)]

use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Named sites on the XDP / TX path (index into [`SPANS`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Site {
    /// `Prog::run` / leased image execution.
    XdpRun = 0,
    /// Claim token + prepare TX (pre-`tx_by_ref`).
    TxEnqueue = 1,
    /// One `esp_wifi_internal_tx_by_ref` call.
    TxDrainOne = 2,
    /// Unused (batch path removed); retained for span index stability.
    TxDrainBatch = 3,
    /// STA/AP RX callback XDP dispatch (Pass/Drop/Tx decision).
    WifiRxDispatch = 4,
    _Count = 5,
}

const SITE_COUNT: usize = Site::_Count as usize;

/// Folded-stack path prefix per site (static labels for host flamegraph).
const SITE_FOLDED: [&str; SITE_COUNT] = [
    "wifi_rx;xdp_run",
    "wifi_rx;tx_enqueue",
    "tx_batch;tx_drain_one",
    "tx_batch;tx_drain_batch",
    "wifi_rx;dispatch",
];

const SITE_NAME: [&str; SITE_COUNT] = [
    "xdp_run",
    "tx_enqueue",
    "tx_drain_one",
    "tx_drain_batch",
    "wifi_rx_dispatch",
];

struct SpanAccum {
    cycles: AtomicU64,
    entries: AtomicU32,
}

#[unsafe(link_section = ".bss")]
static SPANS: [SpanAccum; SITE_COUNT] = [const {
    SpanAccum {
        cycles: AtomicU64::new(0),
        entries: AtomicU32::new(0),
    }
}; SITE_COUNT];

/// Spans / PC hits are off by default: per-packet timer reads and flash-resident
/// accounting on the RX path evict Wi-Fi blob code from cache.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turn span timing and PC-hit sampling on or off (serial `prof on|off`).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Enter a timed span; drop to accumulate `mcycle` delta into [`Site`].
pub struct Span {
    site: Site,
    t0: Option<u64>,
}

impl Span {
    #[inline]
    pub fn enter(site: Site) -> Self {
        Self {
            site,
            t0: ENABLED.load(Ordering::Relaxed).then(read_mcycle),
        }
    }
}

impl Drop for Span {
    #[inline]
    fn drop(&mut self) {
        let Some(t0) = self.t0 else { return };
        let dt = read_mcycle().wrapping_sub(t0);
        let i = self.site as usize;
        SPANS[i].cycles.fetch_add(dt, Ordering::Relaxed);
        SPANS[i].entries.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
fn read_mcycle() -> u64 {
    // Prefer systimer micros — ESP32-C3 traps on standard `mcycle`/`mcycleh`.
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

/// One span accumulator snapshot.
#[derive(Clone, Copy, Debug)]
pub struct SpanSnap {
    pub site: Site,
    pub name: &'static str,
    pub folded: &'static str,
    pub cycles: u64,
    pub entries: u32,
}

/// Copy all span counters (heapless fixed array).
pub fn snapshot_spans() -> [SpanSnap; SITE_COUNT] {
    const SITES: [Site; SITE_COUNT] = [
        Site::XdpRun,
        Site::TxEnqueue,
        Site::TxDrainOne,
        Site::TxDrainBatch,
        Site::WifiRxDispatch,
    ];
    let mut out = [SpanSnap {
        site: Site::XdpRun,
        name: "",
        folded: "",
        cycles: 0,
        entries: 0,
    }; SITE_COUNT];
    for (i, site) in SITES.iter().copied().enumerate() {
        out[i] = SpanSnap {
            site,
            name: SITE_NAME[i],
            folded: SITE_FOLDED[i],
            cycles: SPANS[i].cycles.load(Ordering::Relaxed),
            entries: SPANS[i].entries.load(Ordering::Relaxed),
        };
    }
    out
}

pub fn clear_spans() {
    for s in &SPANS {
        s.cycles.store(0, Ordering::Relaxed);
        s.entries.store(0, Ordering::Relaxed);
    }
}

// ----- PC hit histogram (host-side symbolize → flamegraph) -----

const PC_SLOTS: usize = 256;

struct PcSlot {
    pc: AtomicU32,
    hits: AtomicU32,
}

#[unsafe(link_section = ".bss")]
static PC_HITS: [PcSlot; PC_SLOTS] = [const {
    PcSlot {
        pc: AtomicU32::new(0),
        hits: AtomicU32::new(0),
    }
}; PC_SLOTS];

static PC_OVERFLOW: AtomicU32 = AtomicU32::new(0);
static PC_SAMPLES: AtomicU32 = AtomicU32::new(0);

/// Record a program-counter sample (e.g. from a timer ISR reading MEPC).
///
/// Linear probe; on table full increments [`pc_overflow`] and drops the sample.
pub fn hit_pc(pc: u32) {
    if pc == 0 {
        return;
    }
    PC_SAMPLES.fetch_add(1, Ordering::Relaxed);
    let mut i = (pc as usize).wrapping_mul(2654435761) % PC_SLOTS;
    for _ in 0..PC_SLOTS {
        let slot = &PC_HITS[i];
        let cur = slot.pc.load(Ordering::Acquire);
        if cur == pc {
            slot.hits.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if cur == 0
            && slot
                .pc
                .compare_exchange(0, pc, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            slot.hits.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if cur == 0 {
            // Lost race; retry same slot.
            continue;
        }
        i = (i + 1) % PC_SLOTS;
    }
    PC_OVERFLOW.fetch_add(1, Ordering::Relaxed);
}

/// Sample the return address of the caller (coarse; not a full stack walk).
#[inline(never)]
pub fn hit_here() {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let ra: u32;
    unsafe {
        core::arch::asm!("mv {0}, ra", out(reg) ra, options(nostack, preserves_flags));
    }
    hit_pc(ra);
}

#[derive(Clone, Copy, Debug)]
pub struct PcHit {
    pub pc: u32,
    pub hits: u32,
}

/// Heapless dump of occupied PC slots (order undefined).
pub fn snapshot_pc_hits(out: &mut [PcHit]) -> usize {
    let mut n = 0usize;
    for slot in &PC_HITS {
        if n >= out.len() {
            break;
        }
        let pc = slot.pc.load(Ordering::Relaxed);
        let hits = slot.hits.load(Ordering::Relaxed);
        if pc != 0 && hits != 0 {
            out[n] = PcHit { pc, hits };
            n += 1;
        }
    }
    n
}

pub fn pc_samples() -> u32 {
    PC_SAMPLES.load(Ordering::Relaxed)
}

pub fn pc_overflow() -> u32 {
    PC_OVERFLOW.load(Ordering::Relaxed)
}

pub fn clear_pc_hits() {
    for slot in &PC_HITS {
        slot.pc.store(0, Ordering::Relaxed);
        slot.hits.store(0, Ordering::Relaxed);
    }
    PC_SAMPLES.store(0, Ordering::Relaxed);
    PC_OVERFLOW.store(0, Ordering::Relaxed);
}

pub fn clear_all() {
    clear_spans();
    clear_pc_hits();
}
