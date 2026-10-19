//! Aggregate latency histograms (`feature = "perf-aggregate"`).
//!
//! Pipeline spans keep a stretch histogram. `map_lookup_elem` uses fixed edges
//! and is recorded only with `perf-lookup`. A zero return is `lookup_null`.
//! That value is not a proven miss.

use core::sync::atomic::Ordering;

use portable_atomic::{AtomicU32, AtomicU64};

/// Histogram bucket count (power of two for pairwise merge).
pub const HIST_BUCKETS: usize = 16;

/// Read-only snapshot of one aggregate.
#[derive(Clone, Copy, Debug)]
pub struct AggregateSnapshot {
    pub count: u64,
    pub total_ticks: u64,
    pub min_ticks: u64,
    pub max_ticks: u64,
    pub lo: u64,
    pub width: u64,
    pub buckets: [u64; HIST_BUCKETS],
}

/// One latency aggregate with stretch histogram.
pub struct Aggregate {
    count: AtomicU64,
    total_ticks: AtomicU64,
    min_ticks: AtomicU64,
    max_ticks: AtomicU64,
    /// Bucket base (inclusive).
    lo: AtomicU64,
    /// Bucket width in ticks.
    width: AtomicU64,
    buckets: [AtomicU64; HIST_BUCKETS],
    /// 0 = uninitialized window.
    ready: AtomicU32,
    /// In-flight start tick; 0 = not started.
    start: AtomicU64,
}

impl Aggregate {
    pub const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            min_ticks: AtomicU64::new(u64::MAX),
            max_ticks: AtomicU64::new(0),
            lo: AtomicU64::new(0),
            width: AtomicU64::new(1),
            buckets: [const { AtomicU64::new(0) }; HIST_BUCKETS],
            ready: AtomicU32::new(0),
            start: AtomicU64::new(0),
        }
    }

    #[inline]
    pub(crate) fn start_ticks(&self) -> u64 {
        self.start.load(Ordering::Relaxed)
    }

    #[inline]
    pub(crate) fn begin(&self, now: u64) {
        self.start.store(now.max(1), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn end(&self, now: u64) {
        let t0 = self.start.swap(0, Ordering::Relaxed);
        if t0 == 0 {
            return;
        }
        let dt = now.saturating_sub(t0);
        self.record(dt);
    }

    /// Record a finished duration in ticks (also used for block samples).
    pub fn record(&self, ticks: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ticks.fetch_add(ticks, Ordering::Relaxed);
        let _ = self
            .min_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (ticks < m).then_some(ticks)
            });
        let _ = self
            .max_ticks
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (ticks > m).then_some(ticks)
            });
        self.insert_hist(ticks);
    }

    fn insert_hist(&self, ticks: u64) {
        // The critical section owns the window so a grow cannot tear buckets.
        critical_section::with(|_| {
            if self.ready.load(Ordering::Relaxed) == 0 {
                self.lo.store(ticks, Ordering::Relaxed);
                self.width.store(1, Ordering::Relaxed);
                for b in &self.buckets {
                    b.store(0, Ordering::Relaxed);
                }
                self.ready.store(1, Ordering::Relaxed);
            }
            for _ in 0..96 {
                let lo = self.lo.load(Ordering::Relaxed);
                let width = self.width.load(Ordering::Relaxed).max(1);
                if ticks >= lo {
                    let idx = (ticks - lo) / width;
                    if idx < HIST_BUCKETS as u64 {
                        self.buckets[idx as usize].fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }
                if !self.grow(ticks) {
                    break;
                }
            }
            let lo = self.lo.load(Ordering::Relaxed);
            let idx = if ticks < lo { 0 } else { HIST_BUCKETS - 1 };
            self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        });
    }

    /// Double `width` and slide `lo` on that grid.
    ///
    /// `lo` stays a multiple of `width`. Each old bucket merges into one new
    /// bucket. A sample below `lo` shifts the window down. A sample above it
    /// shifts by at most one bucket so the next width stays aligned.
    /// Returns false when `width` cannot double.
    fn grow(&self, ticks: u64) -> bool {
        let width = self.width.load(Ordering::Relaxed).max(1);
        let Some(new_width) = width.checked_mul(2) else {
            return false;
        };
        let lo = self.lo.load(Ordering::Relaxed);
        debug_assert_eq!(lo % width, 0);
        let unit = lo / width;
        let parity = (unit % 2) as usize;
        let max_shift = (unit as usize).min(HIST_BUCKETS);
        let max_aligned = if max_shift % 2 == parity {
            max_shift
        } else {
            max_shift.saturating_sub(1)
        };
        let shift = if ticks < lo {
            let need = ((lo - ticks + width - 1) / width) as usize;
            let need_aligned = if need % 2 == parity { need } else { need + 1 };
            if need_aligned <= max_aligned {
                need_aligned
            } else {
                max_aligned
            }
        } else if parity == 0 || max_aligned == 0 {
            0
        } else {
            1
        };
        if ticks < lo && shift == 0 {
            return false;
        }
        let mut merged = [0u64; HIST_BUCKETS];
        for i in 0..HIST_BUCKETS {
            let dst = (shift + i) / 2;
            debug_assert!(dst < HIST_BUCKETS);
            merged[dst] = merged[dst].saturating_add(self.buckets[i].load(Ordering::Relaxed));
        }
        for (i, count) in merged.iter().enumerate() {
            self.buckets[i].store(*count, Ordering::Relaxed);
        }
        self.lo
            .store(lo - (shift as u64) * width, Ordering::Relaxed);
        self.width.store(new_width, Ordering::Relaxed);
        true
    }

    pub fn snapshot(&self) -> AggregateSnapshot {
        let min = self.min_ticks.load(Ordering::Relaxed);
        AggregateSnapshot {
            count: self.count.load(Ordering::Relaxed),
            total_ticks: self.total_ticks.load(Ordering::Relaxed),
            min_ticks: if min == u64::MAX { 0 } else { min },
            max_ticks: self.max_ticks.load(Ordering::Relaxed),
            lo: self.lo.load(Ordering::Relaxed),
            width: self.width.load(Ordering::Relaxed).max(1),
            buckets: core::array::from_fn(|i| self.buckets[i].load(Ordering::Relaxed)),
        }
    }

    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.total_ticks.store(0, Ordering::Relaxed);
        self.min_ticks.store(u64::MAX, Ordering::Relaxed);
        self.max_ticks.store(0, Ordering::Relaxed);
        self.lo.store(0, Ordering::Relaxed);
        self.width.store(1, Ordering::Relaxed);
        self.ready.store(0, Ordering::Relaxed);
        self.start.store(0, Ordering::Relaxed);
        for b in &self.buckets {
            b.store(0, Ordering::Relaxed);
        }
    }
}

/// Named aggregates for the mounted program.
///
/// Slots 6 and 7 stay empty here. HAL fills `helper_execution` and `map_operation`.
/// The last three slots are fixed-edge lookup histograms. `width == 0` means
/// [`LOOKUP_EDGE`] version [`LOOKUP_HIST_VERSION`], not a uniform bin width.
pub struct AggregateSet {
    pub rx_processing: Aggregate,
    pub xdp_execution: Aggregate,
    pub xdp_dispatch: Aggregate,
    pub post_action: Aggregate,
    pub tx_submission: Aggregate,
    pub tx_block: Aggregate,
    pub lookup_non_null: FixedHist,
    pub lookup_null: FixedHist,
    pub lookup_timer_empty: FixedHist,
}

/// Lower edge of each lookup bucket, in `rdcycle` counts.
/// Bucket `i` is `[LOOKUP_EDGE[i], LOOKUP_EDGE[i+1])`.
/// The last bucket is `[LOOKUP_EDGE[15], +inf)`.
pub const LOOKUP_HIST_VERSION: u64 = 1;
pub const LOOKUP_EDGE: [u64; HIST_BUCKETS] = [
    0, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536, 131072, 262144, 524288,
];

/// Fixed-edge histogram. Samples are never moved into a new window.
pub struct FixedHist {
    count: AtomicU64,
    total_ticks: AtomicU64,
    min_ticks: AtomicU64,
    max_ticks: AtomicU64,
    buckets: [AtomicU64; HIST_BUCKETS],
}

impl FixedHist {
    pub const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            total_ticks: AtomicU64::new(0),
            min_ticks: AtomicU64::new(u64::MAX),
            max_ticks: AtomicU64::new(0),
            buckets: [const { AtomicU64::new(0) }; HIST_BUCKETS],
        }
    }

    #[cfg(feature = "perf-lookup")]
    pub fn record(&self, ticks: u64) {
        let idx = lookup_bucket(ticks);
        critical_section::with(|_| {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.total_ticks.fetch_add(ticks, Ordering::Relaxed);
            let _ = self
                .min_ticks
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                    (ticks < m).then_some(ticks)
                });
            let _ = self
                .max_ticks
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                    (ticks > m).then_some(ticks)
                });
            self.buckets[idx].fetch_add(1, Ordering::Relaxed);
        });
    }

    pub fn snapshot(&self) -> AggregateSnapshot {
        let min = self.min_ticks.load(Ordering::Relaxed);
        AggregateSnapshot {
            count: self.count.load(Ordering::Relaxed),
            total_ticks: self.total_ticks.load(Ordering::Relaxed),
            min_ticks: if min == u64::MAX { 0 } else { min },
            max_ticks: self.max_ticks.load(Ordering::Relaxed),
            lo: LOOKUP_HIST_VERSION,
            width: 0,
            buckets: core::array::from_fn(|i| self.buckets[i].load(Ordering::Relaxed)),
        }
    }

    pub fn reset(&self) {
        self.count.store(0, Ordering::Relaxed);
        self.total_ticks.store(0, Ordering::Relaxed);
        self.min_ticks.store(u64::MAX, Ordering::Relaxed);
        self.max_ticks.store(0, Ordering::Relaxed);
        for b in &self.buckets {
            b.store(0, Ordering::Relaxed);
        }
    }
}

/// Bucket index for [`LOOKUP_EDGE`]. `ticks` stays in that bucket for the life of the histogram.
#[cfg(feature = "perf-lookup")]
pub fn lookup_bucket(ticks: u64) -> usize {
    let mut idx = 0usize;
    for (i, edge) in LOOKUP_EDGE.iter().enumerate().skip(1) {
        if ticks < *edge {
            return i - 1;
        }
        idx = i;
    }
    idx
}

impl AggregateSet {
    pub const fn new() -> Self {
        Self {
            rx_processing: Aggregate::new(),
            xdp_execution: Aggregate::new(),
            xdp_dispatch: Aggregate::new(),
            post_action: Aggregate::new(),
            tx_submission: Aggregate::new(),
            tx_block: Aggregate::new(),
            lookup_non_null: FixedHist::new(),
            lookup_null: FixedHist::new(),
            lookup_timer_empty: FixedHist::new(),
        }
    }

    #[inline]
    pub(crate) fn now_ticks() -> u64 {
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros()
    }

    pub fn reset_all(&self) {
        self.rx_processing.reset();
        self.xdp_execution.reset();
        self.xdp_dispatch.reset();
        self.post_action.reset();
        self.tx_submission.reset();
        self.tx_block.reset();
        self.lookup_non_null.reset();
        self.lookup_null.reset();
        self.lookup_timer_empty.reset();
    }
}

/// Low 32 bits of the cycle counter. A short interval uses `wrapping_sub`.
#[cfg(feature = "perf-lookup")]
#[inline]
pub(crate) fn lookup_cycles() -> u32 {
    let mut cycles: u32;
    unsafe {
        core::arch::asm!(
            "rdcycle {cycles}",
            cycles = out(reg) cycles,
            options(nomem, nostack)
        );
    }
    cycles
}

/// Record one lookup `dt` after both cycle reads. `returned_null` is the raw pointer result.
#[cfg(feature = "perf-lookup")]
#[inline]
pub(crate) fn record_lookup(dt: u64, returned_null: bool, empty_dt: u64) {
    let set = &crate::xdp::ingress::XDP_PROG.aggregate;
    if returned_null {
        set.lookup_null.record(dt);
    } else {
        set.lookup_non_null.record(dt);
    }
    set.lookup_timer_empty.record(empty_dt);
}
