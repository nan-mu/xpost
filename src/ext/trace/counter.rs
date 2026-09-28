//! Driver Observe counters (`feature = "trace-counter"`).
//!
//! [`Counter`] owns atomics and implements [`Observe`]. The attach target is
//! always [`crate::XDP_PROG`] ([`crate::trace::Trace`]), which forwards here.

use core::sync::atomic::Ordering;

use portable_atomic::{AtomicI32, AtomicU32, AtomicUsize};
use xpost_trait::Observe;

/// Atomic counters for esp-radio Observe + xpost soft-fail paths.
pub struct Counter {
    drop: AtomicUsize,
    tx_fail_bad_buf: AtomicUsize,
    tx_fail_token: AtomicUsize,
    tx_submit_fail: AtomicUsize,
    tx_wifi_last_rc: AtomicI32,
    tx_grp_src: AtomicUsize,
    tx_enq: AtomicUsize,
    tx_enq_fail_claim: AtomicUsize,
    tx_enq_fail_ring: AtomicUsize,
    tx_kick: AtomicUsize,
    tx_poll: AtomicUsize,
    tx_cap_wait: AtomicUsize,
    tx_nomem_wait: AtomicUsize,
    tx_drain_ok: AtomicUsize,
    tx_no_retain: AtomicUsize,
    tx_flush: AtomicUsize,
    tx_air_done: AtomicUsize,
    tx_air_done_fail: AtomicUsize,
    tx_ns_ref: AtomicUsize,
    tx_ns_free: AtomicUsize,
    tx_flow_sample: AtomicUsize,
    tx_log_dropped: AtomicUsize,
    tx_pending_peak: AtomicUsize,
    tx_inflight_peak: AtomicUsize,
    tx_block_max_us: AtomicU32,
    tx_block_gt10ms: AtomicUsize,
    tx_block_gt100ms: AtomicUsize,
    block_since_us: AtomicU32,
    inactive_skip: AtomicUsize,
    reentry: AtomicUsize,
    stack_misalign: AtomicUsize,
    bad_md: AtomicUsize,
    helper_unresolved: AtomicUsize,
    helper_map: AtomicUsize,
    pool_drop_misalign: AtomicUsize,
}

impl Counter {
    pub const fn new() -> Self {
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

    #[inline]
    pub fn drop_count(&self) -> usize {
        self.drop.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn failed_tx_count(&self) -> usize {
        self.failed_tx_bad_buf_count()
            + self.failed_tx_token_count()
            + self.failed_tx_wifi_count()
    }

    #[inline]
    pub fn failed_tx_bad_buf_count(&self) -> usize {
        self.tx_fail_bad_buf.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn failed_tx_token_count(&self) -> usize {
        self.tx_fail_token.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn failed_tx_wifi_count(&self) -> usize {
        self.tx_submit_fail.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn failed_tx_wifi_last_rc(&self) -> i32 {
        self.tx_wifi_last_rc.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn inactive_skip(&self) -> usize {
        self.inactive_skip.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn reentry(&self) -> usize {
        self.reentry.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn stack_misalign(&self) -> usize {
        self.stack_misalign.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn bad_md(&self) -> usize {
        self.bad_md.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn helper_unresolved(&self) -> usize {
        self.helper_unresolved.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn helper_map(&self) -> usize {
        self.helper_map.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn pool_drop_misalign(&self) -> usize {
        self.pool_drop_misalign.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_enq(&self) -> usize {
        self.tx_enq.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_enq_claim(&self) -> usize {
        self.tx_enq_fail_claim.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_enq_ring(&self) -> usize {
        self.tx_enq_fail_ring.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_drain_ok(&self) -> usize {
        self.tx_drain_ok.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_nomem_wait(&self) -> usize {
        self.tx_nomem_wait.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_cap_wait(&self) -> usize {
        self.tx_cap_wait.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_submit_fail(&self) -> usize {
        self.tx_submit_fail.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_polls(&self) -> usize {
        self.tx_poll.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_kicks(&self) -> usize {
        self.tx_kick.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_done(&self) -> usize {
        self.tx_air_done.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_done_fail(&self) -> usize {
        self.tx_air_done_fail.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_grp_src(&self) -> usize {
        self.tx_grp_src.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_ns_ref(&self) -> usize {
        self.tx_ns_ref.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_ns_free(&self) -> usize {
        self.tx_ns_free.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_no_retain(&self) -> usize {
        self.tx_no_retain.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_flush(&self) -> usize {
        self.tx_flush.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_flow_samples(&self) -> usize {
        self.tx_flow_sample.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_log_dropped(&self) -> usize {
        self.tx_log_dropped.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_pending_peak(&self) -> usize {
        self.tx_pending_peak.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_inflight_peak(&self) -> usize {
        self.tx_inflight_peak.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_block_max_us(&self) -> u32 {
        self.tx_block_max_us.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_block_gt10ms(&self) -> usize {
        self.tx_block_gt10ms.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_block_gt100ms(&self) -> usize {
        self.tx_block_gt100ms.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn tx_pend(&self) -> usize {
        esp_radio::wifi::xdp_tx_pending()
    }

    #[inline]
    pub fn tx_inflight(&self) -> usize {
        esp_radio::wifi::xdp_tx_in_flight()
    }

    #[inline]
    pub fn tx_last_rc(&self) -> i32 {
        esp_radio::wifi::xdp_tx_last_drain_rc()
    }

    #[inline]
    pub fn tx_inflight_cap(&self) -> usize {
        esp_radio::wifi::xdp_tx_stats().inflight_cap as usize
    }

    pub fn clear_tx(&self) {
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

    #[inline]
    pub(crate) fn inc_inactive_skip(&self) {
        self.inactive_skip.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn inc_reentry(&self) {
        self.reentry.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn inc_stack_misalign(&self) {
        self.stack_misalign.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn inc_bad_md(&self) {
        self.bad_md.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn inc_helper_unresolved(&self) {
        self.helper_unresolved.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn inc_helper_map(&self) {
        self.helper_map.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub(crate) fn inc_pool_drop_misalign(&self) {
        self.pool_drop_misalign.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    fn note_block_start(&self) {
        let t = esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros() as u32;
        let _ = self.block_since_us.compare_exchange(
            0,
            t.max(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    #[inline]
    fn note_block_end(&self) {
        let since = self.block_since_us.swap(0, Ordering::Relaxed);
        if since == 0 {
            return;
        }
        let now = esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_micros() as u32;
        let dur = now.wrapping_sub(since);
        let _ = self
            .tx_block_max_us
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (dur > m).then_some(dur)
            });
        if dur > 10_000 {
            self.tx_block_gt10ms.fetch_add(1, Ordering::Relaxed);
        }
        if dur > 100_000 {
            self.tx_block_gt100ms.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[inline]
    fn depth_sample(&self) {
        let pend = esp_radio::wifi::xdp_tx_pending();
        let infl = esp_radio::wifi::xdp_tx_in_flight();
        let _ = self
            .tx_pending_peak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (pend > m).then_some(pend)
            });
        let _ = self
            .tx_inflight_peak
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |m| {
                (infl > m).then_some(infl)
            });
    }
}

impl Observe for Counter {
    fn on_drop(&self) {
        self.drop.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_fail_bad_buf(&self) {
        self.tx_fail_bad_buf.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_fail_token(&self) {
        self.tx_fail_token.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_grp_src(&self) {
        self.tx_grp_src.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_enqueued(&self) {
        self.tx_enq.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_enq_fail_claim(&self) {
        self.tx_enq_fail_claim.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_enq_fail_ring(&self) {
        self.tx_enq_fail_ring.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_kick(&self) {
        self.tx_kick.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_poll(&self) {
        self.tx_poll.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_cap_wait(&self) {
        self.tx_cap_wait.fetch_add(1, Ordering::Relaxed);
        self.note_block_start();
    }

    fn on_tx_nomem_wait(&self) {
        self.tx_nomem_wait.fetch_add(1, Ordering::Relaxed);
        self.note_block_start();
    }

    fn on_tx_submit_ok(&self) {
        self.tx_drain_ok.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_submit_fail(&self) {
        self.tx_submit_fail.fetch_add(1, Ordering::Relaxed);
        self.tx_wifi_last_rc.store(
            esp_radio::wifi::xdp_tx_last_drain_rc(),
            Ordering::Relaxed,
        );
    }

    fn on_tx_no_retain(&self) {
        self.tx_no_retain.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_flush(&self) {
        self.tx_flush.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_air_done(&self) {
        self.tx_air_done.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_air_done_fail(&self) {
        self.tx_air_done_fail.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_netstack_ref(&self) {
        self.tx_ns_ref.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_netstack_free(&self) {
        self.tx_ns_free.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_depth_sample(&self) {
        self.depth_sample();
    }

    fn on_tx_block_sample(&self) {
        self.note_block_end();
    }

    fn on_tx_flow_sample(&self) {
        self.tx_flow_sample.fetch_add(1, Ordering::Relaxed);
    }

    fn on_tx_log_dropped(&self) {
        self.tx_log_dropped.fetch_add(1, Ordering::Relaxed);
    }
}
