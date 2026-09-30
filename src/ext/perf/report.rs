//! Compile-time feature-gated binary perf report (`report` serial command).
//!
//! Sections in order **C → A → G → T → S → P** (omit absent features).
//! Payload length uniquely identifies the feature mask (see host length table).

use core::convert::Infallible;

#[cfg(feature = "perf-counter")]
pub const COUNTER_WIRE_LEN: usize = 184;
#[cfg(feature = "perf-aggregate")]
pub const AGGREGATE_WIRE_LEN: usize = 8 * 176;
#[cfg(feature = "perf-gauge")]
pub const GAUGE_WIRE_LEN: usize = 32;
#[cfg(feature = "perf-trace")]
pub const TRACE_WIRE_LEN: usize = crate::ext::perf::trace::WIRE_LEN;
#[cfg(feature = "perf-span")]
pub const SPAN_WIRE_LEN: usize = crate::ext::perf::prof::span::WIRE_LEN;
#[cfg(feature = "perf-pchit")]
pub const PCHIT_WIRE_LEN: usize = crate::ext::perf::prof::pchit::WIRE_LEN;

/// Total payload bytes for the features compiled into this firmware image.
pub const fn report_payload_len() -> usize {
    let mut n = 0usize;
    #[cfg(feature = "perf-counter")]
    {
        n += COUNTER_WIRE_LEN;
    }
    #[cfg(feature = "perf-aggregate")]
    {
        n += AGGREGATE_WIRE_LEN;
    }
    #[cfg(feature = "perf-gauge")]
    {
        n += GAUGE_WIRE_LEN;
    }
    #[cfg(feature = "perf-trace")]
    {
        n += TRACE_WIRE_LEN;
    }
    #[cfg(feature = "perf-span")]
    {
        n += SPAN_WIRE_LEN;
    }
    #[cfg(feature = "perf-pchit")]
    {
        n += PCHIT_WIRE_LEN;
    }
    n
}

/// Reset every compiled-in perf backend.
pub fn clear_perf() {
    #[cfg(feature = "perf-counter")]
    crate::ext::perf::counter().reset_all();
    #[cfg(feature = "perf-aggregate")]
    crate::ext::perf::aggregate().reset_all();
    #[cfg(feature = "perf-gauge")]
    crate::ext::perf::gauge().reset_all();
    #[cfg(feature = "perf-trace")]
    crate::ext::perf::trace().clear();
    #[cfg(feature = "perf-span")]
    crate::ext::perf::prof::span().reset_all();
    #[cfg(feature = "perf-pchit")]
    crate::ext::perf::prof::pchit().reset_all();
}

/// Encode all enabled sections into `write` (raw LE bytes, no framing).
pub fn report_perf(mut write: impl FnMut(&[u8]) -> Result<(), Infallible>) {
    let _ = write;
    #[cfg(feature = "perf-counter")]
    {
        let mut buf = [0u8; COUNTER_WIRE_LEN];
        encode_counter(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-aggregate")]
    {
        let mut buf = [0u8; AGGREGATE_WIRE_LEN];
        encode_aggregate(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-gauge")]
    {
        let mut buf = [0u8; GAUGE_WIRE_LEN];
        encode_gauge(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-trace")]
    {
        let mut buf = [0u8; TRACE_WIRE_LEN];
        encode_trace(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-span")]
    {
        let mut buf = [0u8; SPAN_WIRE_LEN];
        crate::ext::perf::prof::span().encode_wire(&mut buf);
        let _ = write(&buf);
    }
    #[cfg(feature = "perf-pchit")]
    {
        let mut buf = [0u8; PCHIT_WIRE_LEN];
        crate::ext::perf::prof::pchit().encode_wire(&mut buf);
        let _ = write(&buf);
    }
}

#[cfg(feature = "perf-counter")]
fn encode_counter(out: &mut [u8; COUNTER_WIRE_LEN]) {
    let t = crate::ext::perf::counter();
    let mut o = 0usize;
    let put_u32 = |v: u32, out: &mut [u8], o: &mut usize| {
        out[*o..*o + 4].copy_from_slice(&v.to_le_bytes());
        *o += 4;
    };
    let put_usz = |v: usize, out: &mut [u8], o: &mut usize| {
        put_u32(v as u32, out, o);
    };
    // Must match host `COUNTER_FIELDS` order.
    put_usz(t.rx_packets(), out, &mut o);
    put_usz(t.xdp_invocations(), out, &mut o);
    put_usz(t.xdp_pass(), out, &mut o);
    put_usz(t.xdp_drop(), out, &mut o);
    put_usz(t.xdp_tx(), out, &mut o);
    put_usz(t.xdp_aborted(), out, &mut o);
    put_usz(t.rx_delivered(), out, &mut o);
    put_usz(t.rx_discarded(), out, &mut o);
    put_usz(t.rx_queue_overflow(), out, &mut o);
    put_usz(t.tx_submitted(), out, &mut o);
    put_usz(t.tx_failed(), out, &mut o);
    put_usz(t.tx_completed(), out, &mut o);
    put_usz(t.runtime_errors(), out, &mut o);
    put_usz(t.tx_fail_bad_buf(), out, &mut o);
    put_usz(t.tx_fail_token(), out, &mut o);
    put_usz(t.tx_grp_src(), out, &mut o);
    put_usz(t.tx_enq(), out, &mut o);
    put_usz(t.tx_enq_fail_claim(), out, &mut o);
    put_usz(t.tx_enq_fail_ring(), out, &mut o);
    put_usz(t.tx_kick(), out, &mut o);
    put_usz(t.tx_poll(), out, &mut o);
    put_usz(t.tx_cap_wait(), out, &mut o);
    put_usz(t.tx_nomem_wait(), out, &mut o);
    put_usz(t.tx_no_retain(), out, &mut o);
    put_usz(t.tx_flush(), out, &mut o);
    put_usz(t.tx_air_done_fail(), out, &mut o);
    put_usz(t.tx_ns_ref(), out, &mut o);
    put_usz(t.tx_ns_free(), out, &mut o);
    put_usz(t.tx_flow_sample(), out, &mut o);
    put_usz(t.tx_block_gt10ms(), out, &mut o);
    put_usz(t.tx_block_gt100ms(), out, &mut o);
    put_usz(t.inactive_skip(), out, &mut o);
    put_usz(t.reentry(), out, &mut o);
    put_usz(t.stack_misalign(), out, &mut o);
    put_usz(t.bad_md(), out, &mut o);
    put_usz(t.helper_unresolved(), out, &mut o);
    put_usz(t.helper_map(), out, &mut o);
    put_usz(t.pool_drop_misalign(), out, &mut o);
    out[o..o + 4].copy_from_slice(&t.failed_tx_wifi_last_rc().to_le_bytes());
    o += 4;
    put_usz(t.tx_pend(), out, &mut o);
    put_usz(t.tx_inflight(), out, &mut o);
    put_usz(t.tx_inflight_cap(), out, &mut o);
    put_usz(t.tx_pending_peak(), out, &mut o);
    put_usz(t.tx_inflight_peak(), out, &mut o);
    put_u32(t.tx_block_max_us(), out, &mut o);
    // block_since_us is internal; export 0 (not a public getter).
    put_u32(0, out, &mut o);
    debug_assert_eq!(o, COUNTER_WIRE_LEN);
}

#[cfg(feature = "perf-aggregate")]
fn encode_aggregate(out: &mut [u8; AGGREGATE_WIRE_LEN]) {
    let a = crate::ext::perf::aggregate();
    let snaps = [
        a.rx_processing.snapshot(),
        a.xdp_execution.snapshot(),
        a.xdp_dispatch.snapshot(),
        a.post_action.snapshot(),
        a.tx_submission.snapshot(),
        a.tx_block.snapshot(),
        a.helper_execution.snapshot(),
        a.map_operation.snapshot(),
    ];
    for (i, snap) in snaps.iter().enumerate() {
        let off = i * 176;
        let mut o = off;
        for v in [
            snap.count,
            snap.total_ticks,
            snap.min_ticks,
            snap.max_ticks,
            snap.lo,
            snap.width,
        ] {
            out[o..o + 8].copy_from_slice(&v.to_le_bytes());
            o += 8;
        }
        for b in snap.buckets {
            out[o..o + 8].copy_from_slice(&b.to_le_bytes());
            o += 8;
        }
        debug_assert_eq!(o, off + 176);
    }
}

#[cfg(feature = "perf-gauge")]
fn encode_gauge(out: &mut [u8; GAUGE_WIRE_LEN]) {
    let g = crate::ext::perf::gauge();
    let vals = [
        g.rx_queue_depth.current() as u32,
        g.rx_queue_depth.peak() as u32,
        g.tx_queue_depth.current() as u32,
        g.tx_queue_depth.peak() as u32,
        g.tx_inflight.current() as u32,
        g.tx_inflight.peak() as u32,
        g.trace_buffer_usage.current() as u32,
        g.trace_buffer_usage.peak() as u32,
    ];
    for (i, v) in vals.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

#[cfg(feature = "perf-trace")]
fn encode_trace(out: &mut [u8; TRACE_WIRE_LEN]) {
    let t = crate::ext::perf::trace();
    let mut radio = [crate::ext::perf::Event::default(); crate::ext::perf::TRACE_CAP];
    let mut app = [crate::ext::perf::Event::default(); crate::ext::perf::trace::APP_CAP];
    let nr = t.snapshot_radio(&mut radio);
    let na = t.snapshot_app(&mut app);
    out[0..4].copy_from_slice(&(nr as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(t.dropped() as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(na as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(t.app_dropped() as u32).to_le_bytes());
    let radio_base = 16;
    for (i, ev) in radio.iter().enumerate() {
        let off = radio_base + i * 16;
        out[off] = ev.kind;
        out[off + 1] = 0;
        out[off + 2] = 0;
        out[off + 3] = 0;
        out[off + 4..off + 8].copy_from_slice(&ev.arg0.to_le_bytes());
        out[off + 8..off + 12].copy_from_slice(&ev.arg1.to_le_bytes());
        out[off + 12..off + 16].copy_from_slice(&ev.t_us.to_le_bytes());
    }
    let app_base = radio_base + crate::ext::perf::TRACE_CAP * 16;
    for (i, ev) in app.iter().enumerate() {
        let off = app_base + i * 16;
        out[off] = ev.kind;
        out[off + 1] = 0;
        out[off + 2] = 0;
        out[off + 3] = 0;
        out[off + 4..off + 8].copy_from_slice(&ev.arg0.to_le_bytes());
        out[off + 8..off + 12].copy_from_slice(&ev.arg1.to_le_bytes());
        out[off + 12..off + 16].copy_from_slice(&ev.t_us.to_le_bytes());
    }
}
