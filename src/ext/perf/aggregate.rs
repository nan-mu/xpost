//! Aggregate latency + stretchable histogram (`feature = "perf-aggregate"`).

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
        // Initialize window on first sample.
        if self
            .ready
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            self.lo.store(ticks, Ordering::Relaxed);
            self.width.store(1, Ordering::Relaxed);
        }

        // Stretch until `ticks` fits. Critical section keeps merge consistent.
        critical_section::with(|_| {
            loop {
                let lo = self.lo.load(Ordering::Relaxed);
                let width = self.width.load(Ordering::Relaxed).max(1);
                let hi = lo.saturating_add(width.saturating_mul(HIST_BUCKETS as u64));
                if ticks >= lo && ticks < hi {
                    let idx = ((ticks - lo) / width) as usize;
                    let idx = idx.min(HIST_BUCKETS - 1);
                    self.buckets[idx].fetch_add(1, Ordering::Relaxed);
                    return;
                }
                if ticks < lo {
                    self.expand_down(lo, width);
                } else {
                    self.expand_up(width);
                }
            }
        });
    }

    /// Merge pairwise, double width (range grows upward from same `lo`).
    fn expand_up(&self, width: u64) {
        let mut merged = [0u64; HIST_BUCKETS];
        for i in 0..(HIST_BUCKETS / 2) {
            merged[i] = self.buckets[2 * i].load(Ordering::Relaxed)
                + self.buckets[2 * i + 1].load(Ordering::Relaxed);
        }
        for i in 0..HIST_BUCKETS {
            self.buckets[i].store(merged[i], Ordering::Relaxed);
        }
        self.width
            .store(width.saturating_mul(2).max(1), Ordering::Relaxed);
    }

    /// Shift content into upper half, lower `lo`, then merge if needed via loop.
    fn expand_down(&self, lo: u64, width: u64) {
        let half = HIST_BUCKETS / 2;
        let shift = (half as u64).saturating_mul(width);
        let new_lo = lo.saturating_sub(shift);
        // Move buckets[0..half) → buckets[half..N)
        for i in (0..half).rev() {
            let v = self.buckets[i].swap(0, Ordering::Relaxed);
            self.buckets[i + half].store(
                self.buckets[i + half].load(Ordering::Relaxed) + v,
                Ordering::Relaxed,
            );
        }
        self.lo.store(new_lo, Ordering::Relaxed);
        // If still too narrow after one shift, expand_up next loop iteration.
        // When new sample is still below new_lo (shift saturated at 0), double width.
        if new_lo == lo {
            self.expand_up(width);
            // After expand_up, also move lo down by old half-range in new units:
            // width doubled; subtract half*new_width equivalent — keep simple: set lo=0.
            if lo > 0 {
                // Fold: treat as growing both ways by resetting lo toward 0.
                let w = self.width.load(Ordering::Relaxed).max(1);
                let cur_lo = self.lo.load(Ordering::Relaxed);
                if cur_lo > 0 {
                    // One more down-shift in new width units.
                    let half_span = (half as u64).saturating_mul(w);
                    self.lo.store(cur_lo.saturating_sub(half_span), Ordering::Relaxed);
                    for i in (0..half).rev() {
                        let v = self.buckets[i].swap(0, Ordering::Relaxed);
                        self.buckets[i + half].store(
                            self.buckets[i + half].load(Ordering::Relaxed) + v,
                            Ordering::Relaxed,
                        );
                    }
                }
            }
        }
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
pub struct AggregateSet {
    pub rx_processing: Aggregate,
    pub xdp_execution: Aggregate,
    pub xdp_dispatch: Aggregate,
    pub post_action: Aggregate,
    pub tx_submission: Aggregate,
    pub tx_block: Aggregate,
    pub helper_execution: Aggregate,
    pub map_operation: Aggregate,
    helper_start: AtomicU64,
    map_start: AtomicU64,
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
            helper_execution: Aggregate::new(),
            map_operation: Aggregate::new(),
            helper_start: AtomicU64::new(0),
            map_start: AtomicU64::new(0),
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
        self.helper_execution.reset();
        self.map_operation.reset();
        self.helper_start.store(0, Ordering::Relaxed);
        self.map_start.store(0, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn helper_enter(&self) {
        self.helper_start
            .store(Self::now_ticks().max(1), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn helper_leave(&self) {
        let t0 = self.helper_start.swap(0, Ordering::Relaxed);
        if t0 == 0 {
            return;
        }
        let now = Self::now_ticks();
        self.helper_execution.record(now.saturating_sub(t0));
    }

    #[inline]
    pub(crate) fn map_enter(&self) {
        self.map_start
            .store(Self::now_ticks().max(1), Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn map_leave(&self) {
        let t0 = self.map_start.swap(0, Ordering::Relaxed);
        if t0 == 0 {
            return;
        }
        let now = Self::now_ticks();
        self.map_operation.record(now.saturating_sub(t0));
    }
}

#[inline]
pub(crate) fn helper_enter() {
    crate::xdp::ingress::XDP_PROG.aggregate.helper_enter();
}

#[inline]
pub(crate) fn helper_leave() {
    crate::xdp::ingress::XDP_PROG.aggregate.helper_leave();
}

#[inline]
pub(crate) fn map_enter() {
    crate::xdp::ingress::XDP_PROG.aggregate.map_enter();
}

#[inline]
pub(crate) fn map_leave() {
    crate::xdp::ingress::XDP_PROG.aggregate.map_leave();
}
