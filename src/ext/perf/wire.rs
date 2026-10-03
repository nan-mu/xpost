//! [`EspRadioTrace`](xpost_type::EspRadioTrace) on the mounted ingress program.

use xpost_type::{EspRadioTrace, XdpAction};

use crate::xdp::ingress::XdpProg;

#[cfg(feature = "perf-trace")]
use crate::ext::perf::trace::EventKind;

impl EspRadioTrace for XdpProg {
    fn rx_packets(&self, len: usize) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_rx_packets();
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            self.aggregate.rx_processing.begin(now);
            self.aggregate.xdp_dispatch.begin(now);
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::RxBegin, len as u32, 0);
        let _ = len;
    }

    fn xdp_invocations(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_xdp_invocations();
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            self.aggregate.xdp_dispatch.end(now);
            self.aggregate.xdp_execution.begin(now);
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::XdpBegin, 0, 0);
    }

    fn xdp_action(&self, action: XdpAction) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_xdp_action(action);
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            let t0 = self.aggregate.xdp_execution.start_ticks();
            self.aggregate.xdp_execution.end(now);
            self.aggregate.post_action.begin(now);
            #[cfg(feature = "perf-trace")]
            if t0 != 0 {
                self.trace
                    .maybe_slow(now.saturating_sub(t0) as u32, action as i32 as u32);
            }
            #[cfg(not(feature = "perf-trace"))]
            let _ = t0;
        }
        #[cfg(feature = "perf-trace")]
        self.trace
            .push(EventKind::XdpEnd, action as i32 as u32, 0);
        #[cfg(all(feature = "perf-trace", not(feature = "perf-aggregate")))]
        let _ = action;
    }

    fn rx_delivered(&self, queue_depth: usize) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_rx_delivered();
        #[cfg(feature = "perf-gauge")]
        self.gauge.rx_queue_depth.set(queue_depth);
        #[cfg(feature = "perf-trace")]
        self.trace
            .push(EventKind::RxDelivered, queue_depth as u32, 0);
        let _ = queue_depth;
    }

    fn rx_discarded(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_rx_discarded();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::RxDiscarded, 0, 0);
    }

    fn rx_queue_overflow(&self, queue_depth: usize) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_rx_queue_overflow();
        #[cfg(feature = "perf-gauge")]
        self.gauge.rx_queue_depth.set(queue_depth);
        #[cfg(feature = "perf-trace")]
        self.trace
            .push(EventKind::RxOverflow, queue_depth as u32, 0);
        let _ = queue_depth;
    }

    fn rx_end(&self) {
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            self.aggregate.post_action.end(now);
            self.aggregate.rx_processing.end(now);
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::RxEnd, 0, 0);
    }

    fn tx_submission_begin(&self) {
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            self.aggregate.tx_submission.begin(now);
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxSubBegin, 0, 0);
    }

    fn tx_submitted(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_submitted();
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            self.aggregate.tx_submission.end(now);
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxSubmitted, 0, 0);
    }

    fn tx_failed(&self, rc: i32) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_failed(rc);
        #[cfg(feature = "perf-aggregate")]
        {
            let now = crate::ext::perf::aggregate::AggregateSet::now_ticks();
            self.aggregate.tx_submission.end(now);
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxFailed, rc as u32, 0);
        #[cfg(not(any(feature = "perf-counter", feature = "perf-trace")))]
        let _ = rc;
    }

    fn tx_completed(&self, ok: bool) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_completed(ok);
        #[cfg(feature = "perf-trace")]
        self.trace
            .push(EventKind::TxCompleted, u32::from(ok), 0);
    }

    fn tx_fail_bad_buf(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_fail_bad_buf();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxFailBadBuf, 0, 0);
    }

    fn tx_fail_token(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_fail_token();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxFailToken, 0, 0);
    }

    fn tx_grp_src(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_grp_src();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxGrpSrc, 0, 0);
    }

    fn tx_enq(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_enq();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxEnq, 0, 0);
    }

    fn tx_enq_fail_claim(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_enq_fail_claim();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxEnqFailClaim, 0, 0);
    }

    fn tx_enq_fail_ring(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_enq_fail_ring();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxEnqFailRing, 0, 0);
    }

    fn tx_kick(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_kick();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxKick, 0, 0);
    }

    fn tx_poll(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_poll();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxPoll, 0, 0);
    }

    fn tx_cap_wait(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_cap_wait();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxCapWait, 0, 0);
    }

    fn tx_nomem_wait(&self, rc: i32) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_nomem_wait(rc);
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxNomemWait, rc as u32, 0);
        #[cfg(not(any(feature = "perf-counter", feature = "perf-trace")))]
        let _ = rc;
    }

    fn tx_no_retain(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_no_retain();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxNoRetain, 0, 0);
    }

    fn tx_flush(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_flush();
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxFlush, 0, 0);
    }

    fn tx_ns_ref(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_ns_ref();
    }

    fn tx_ns_free(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_ns_free();
    }

    fn tx_depth(&self, pending: usize, inflight: usize) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_depth(pending, inflight);
        #[cfg(feature = "perf-gauge")]
        {
            self.gauge.tx_queue_depth.set(pending);
            self.gauge.tx_inflight.set(inflight);
        }
        let _ = (pending, inflight);
    }

    fn tx_inflight_cap(&self, cap: usize) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_inflight_cap(cap);
        let _ = cap;
    }

    fn tx_block(&self) {
        #[cfg(feature = "perf-counter")]
        if let Some(us) = self.counter.note_block_end() {
            #[cfg(feature = "perf-aggregate")]
            self.aggregate.tx_block.record(us as u64);
            let _ = us;
        }
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::TxBlock, 0, 0);
    }

    fn tx_flow_sample(&self) {
        #[cfg(feature = "perf-counter")]
        self.counter.bump_tx_flow_sample();
    }

    fn span_begin(&self, site: xpost_type::ProfSite) {
        #[cfg(feature = "perf-span")]
        self.span.begin(site);
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::SpanBegin, site as u32, 0);
        let _ = site;
    }

    fn span_end(&self, site: xpost_type::ProfSite) {
        #[cfg(feature = "perf-span")]
        self.span.end(site);
        #[cfg(feature = "perf-trace")]
        self.trace.push(EventKind::SpanEnd, site as u32, 0);
        let _ = site;
    }

    fn pc_hit(&self, pc: u32) {
        #[cfg(feature = "perf-pchit")]
        self.pchit.hit(pc);
        let _ = pc;
    }
}
