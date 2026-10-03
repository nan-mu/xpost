//! Atomic counters behind `feature = "trace-counter"`.
//!
//! Written by esp-radio [`Observe`](esp_radio::wifi::Observe) hooks and xpost
//! soft-fail paths; read via [`super::TraceState`] methods.

use portable_atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering};

/// Process-wide counter storage (driver Observe + helper/ingress soft-fail).
pub(super) struct Counters {
    // --- A. XDP primitive ---
    pub drop: AtomicUsize,
    pub tx_fail_bad_buf: AtomicUsize,
    pub tx_fail_token: AtomicUsize,
    pub tx_submit_fail: AtomicUsize,
    pub tx_wifi_last_rc: AtomicI32,

    // --- B. xdp_tx ---
    pub tx_grp_src: AtomicUsize,
    pub tx_enq: AtomicUsize,
    pub tx_enq_fail_claim: AtomicUsize,
    pub tx_enq_fail_ring: AtomicUsize,
    pub tx_kick: AtomicUsize,
    pub tx_poll: AtomicUsize,
    pub tx_cap_wait: AtomicUsize,
    pub tx_nomem_wait: AtomicUsize,
    pub tx_drain_ok: AtomicUsize,
    pub tx_no_retain: AtomicUsize,
    pub tx_flush: AtomicUsize,
    pub tx_air_done: AtomicUsize,
    pub tx_air_done_fail: AtomicUsize,
    pub tx_ns_ref: AtomicUsize,
    pub tx_ns_free: AtomicUsize,
    pub tx_flow_sample: AtomicUsize,
    pub tx_log_dropped: AtomicUsize,
    pub tx_pending_peak: AtomicUsize,
    pub tx_inflight_peak: AtomicUsize,
    pub tx_block_max_us: AtomicU32,
    pub tx_block_gt10ms: AtomicUsize,
    pub tx_block_gt100ms: AtomicUsize,
    /// Local block-start µs for [`on_tx_block_sample`](esp_radio::wifi::Observe::on_tx_block_sample).
    pub(super) block_since_us: AtomicU32,

    // --- xpost soft-fail ---
    pub inactive_skip: AtomicUsize,
    pub reentry: AtomicUsize,
    pub stack_misalign: AtomicUsize,
    pub bad_md: AtomicUsize,
    pub helper_unresolved: AtomicUsize,
    pub helper_map: AtomicUsize,
    pub pool_drop_misalign: AtomicUsize,
}

impl Counters {
    pub(super) const fn new() -> Self {
        Self {
            drop: AtomicUsize::new(0),
            tx_fail_bad_buf: AtomicUsize::new(0),
            tx_fail_token: AtomicUsize::new(0),
            tx_submit_fail: AtomicUsize::new(0),
            tx_wifi_last_rc: AtomicI32::new(0),
            tx_grp_src: AtomicUsize::new(0),
            tx_enq: AtomicUsize::new(0),
            tx_enq_fail_claim: AtomicUsize::new(0),
            tx_enq_fail_ring: AtomicUsize::new(0),
            tx_kick: AtomicUsize::new(0),
            tx_poll: AtomicUsize::new(0),
            tx_cap_wait: AtomicUsize::new(0),
            tx_nomem_wait: AtomicUsize::new(0),
            tx_drain_ok: AtomicUsize::new(0),
            tx_no_retain: AtomicUsize::new(0),
            tx_flush: AtomicUsize::new(0),
            tx_air_done: AtomicUsize::new(0),
            tx_air_done_fail: AtomicUsize::new(0),
            tx_ns_ref: AtomicUsize::new(0),
            tx_ns_free: AtomicUsize::new(0),
            tx_flow_sample: AtomicUsize::new(0),
            tx_log_dropped: AtomicUsize::new(0),
            tx_pending_peak: AtomicUsize::new(0),
            tx_inflight_peak: AtomicUsize::new(0),
            tx_block_max_us: AtomicU32::new(0),
            tx_block_gt10ms: AtomicUsize::new(0),
            tx_block_gt100ms: AtomicUsize::new(0),
            block_since_us: AtomicU32::new(0),
            inactive_skip: AtomicUsize::new(0),
            reentry: AtomicUsize::new(0),
            stack_misalign: AtomicUsize::new(0),
            bad_md: AtomicUsize::new(0),
            helper_unresolved: AtomicUsize::new(0),
            helper_map: AtomicUsize::new(0),
            pool_drop_misalign: AtomicUsize::new(0),
        }
    }

    pub(super) fn clear_tx(&self) {
        self.tx_fail_bad_buf.store(0, Ordering::Relaxed);
        self.tx_fail_token.store(0, Ordering::Relaxed);
        self.tx_submit_fail.store(0, Ordering::Relaxed);
        self.tx_wifi_last_rc.store(0, Ordering::Relaxed);
        self.tx_grp_src.store(0, Ordering::Relaxed);
        self.tx_enq.store(0, Ordering::Relaxed);
        self.tx_enq_fail_claim.store(0, Ordering::Relaxed);
        self.tx_enq_fail_ring.store(0, Ordering::Relaxed);
        self.tx_kick.store(0, Ordering::Relaxed);
        self.tx_poll.store(0, Ordering::Relaxed);
        self.tx_cap_wait.store(0, Ordering::Relaxed);
        self.tx_nomem_wait.store(0, Ordering::Relaxed);
        self.tx_drain_ok.store(0, Ordering::Relaxed);
        self.tx_no_retain.store(0, Ordering::Relaxed);
        self.tx_flush.store(0, Ordering::Relaxed);
        self.tx_air_done.store(0, Ordering::Relaxed);
        self.tx_air_done_fail.store(0, Ordering::Relaxed);
        self.tx_ns_ref.store(0, Ordering::Relaxed);
        self.tx_ns_free.store(0, Ordering::Relaxed);
        self.tx_flow_sample.store(0, Ordering::Relaxed);
        self.tx_log_dropped.store(0, Ordering::Relaxed);
        self.tx_pending_peak.store(0, Ordering::Relaxed);
        self.tx_inflight_peak.store(0, Ordering::Relaxed);
        self.tx_block_max_us.store(0, Ordering::Relaxed);
        self.tx_block_gt10ms.store(0, Ordering::Relaxed);
        self.tx_block_gt100ms.store(0, Ordering::Relaxed);
        self.block_since_us.store(0, Ordering::Relaxed);
    }
}
