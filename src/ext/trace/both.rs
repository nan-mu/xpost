//! Combined counter + timestamp Prog when both trace features are enabled.

use core::ops::Deref;

use esp_radio::wifi::{Action, Observe, Prog};

use super::{counter::Counter, timestamp::Timestamp};
use crate::xdp::ingress::run::ingress_run;

/// Single attach target: [`Counter`] Observe + [`Timestamp`] helper ring.
pub struct Both {
    counter: Counter,
    timestamp: Timestamp,
}

impl Both {
    pub const fn new() -> Self {
        Self {
            counter: Counter::new(),
            timestamp: Timestamp::new(),
        }
    }

    #[inline]
    pub fn helper_enter(&self, id: usize) {
        self.timestamp.helper_enter(id);
    }

    #[inline]
    pub fn helper_leave(&self, id: usize) {
        self.timestamp.helper_leave(id);
    }

    #[inline]
    pub fn snapshot_timestamps(
        &self,
        out: &mut [super::timestamp::Event],
    ) -> usize {
        self.timestamp.snapshot_timestamps(out)
    }

    #[inline]
    pub fn clear_timestamps(&self) {
        self.timestamp.clear_timestamps();
    }

    #[inline]
    pub fn timestamp_len(&self) -> usize {
        self.timestamp.timestamp_len()
    }

    #[inline]
    pub fn timestamp_dropped(&self) -> usize {
        self.timestamp.timestamp_dropped()
    }
}

impl Deref for Both {
    type Target = Counter;

    #[inline]
    fn deref(&self) -> &Counter {
        &self.counter
    }
}

impl Prog for Both {
    #[esp_hal::ram]
    fn run<'a>(&self, frame: &'a mut [u8]) -> (Action, &'a mut [u8]) {
        ingress_run(frame)
    }
}

impl Observe for Both {
    fn on_drop(&self) {
        Observe::on_drop(&self.counter);
    }

    fn on_tx_fail_bad_buf(&self) {
        Observe::on_tx_fail_bad_buf(&self.counter);
    }

    fn on_tx_fail_token(&self) {
        Observe::on_tx_fail_token(&self.counter);
    }

    fn on_tx_grp_src(&self) {
        Observe::on_tx_grp_src(&self.counter);
    }

    fn on_tx_enqueued(&self) {
        Observe::on_tx_enqueued(&self.counter);
    }

    fn on_tx_enq_fail_claim(&self) {
        Observe::on_tx_enq_fail_claim(&self.counter);
    }

    fn on_tx_enq_fail_ring(&self) {
        Observe::on_tx_enq_fail_ring(&self.counter);
    }

    fn on_tx_kick(&self) {
        Observe::on_tx_kick(&self.counter);
    }

    fn on_tx_poll(&self) {
        Observe::on_tx_poll(&self.counter);
    }

    fn on_tx_cap_wait(&self) {
        Observe::on_tx_cap_wait(&self.counter);
    }

    fn on_tx_nomem_wait(&self) {
        Observe::on_tx_nomem_wait(&self.counter);
    }

    fn on_tx_submit_ok(&self) {
        Observe::on_tx_submit_ok(&self.counter);
    }

    fn on_tx_submit_fail(&self) {
        Observe::on_tx_submit_fail(&self.counter);
    }

    fn on_tx_no_retain(&self) {
        Observe::on_tx_no_retain(&self.counter);
    }

    fn on_tx_flush(&self) {
        Observe::on_tx_flush(&self.counter);
    }

    fn on_tx_air_done(&self) {
        Observe::on_tx_air_done(&self.counter);
    }

    fn on_tx_air_done_fail(&self) {
        Observe::on_tx_air_done_fail(&self.counter);
    }

    fn on_tx_netstack_ref(&self) {
        Observe::on_tx_netstack_ref(&self.counter);
    }

    fn on_tx_netstack_free(&self) {
        Observe::on_tx_netstack_free(&self.counter);
    }

    fn on_tx_depth_sample(&self) {
        Observe::on_tx_depth_sample(&self.counter);
    }

    fn on_tx_block_sample(&self) {
        Observe::on_tx_block_sample(&self.counter);
    }

    fn on_tx_flow_sample(&self) {
        Observe::on_tx_flow_sample(&self.counter);
    }

    fn on_tx_log_dropped(&self) {
        Observe::on_tx_log_dropped(&self.counter);
    }
}
