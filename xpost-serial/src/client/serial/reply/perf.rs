//! `get perf` framing + binary payload decode (matches `xpost::ext::perf`).

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

pub const COUNTER_WIRE_LEN: usize = 184;
pub const AGGREGATE_WIRE_LEN: usize = 8 * 176;
pub const GAUGE_WIRE_LEN: usize = 32;
pub const TRACE_CAP: usize = 256;
pub const TRACE_APP_CAP: usize = 64;
pub const TRACE_WIRE_LEN: usize = 16 + TRACE_CAP * 16 + TRACE_APP_CAP * 16;

pub const MASK_C: u8 = 1 << 0;
pub const MASK_A: u8 = 1 << 1;
pub const MASK_G: u8 = 1 << 2;
pub const MASK_T: u8 = 1 << 3;

const SECTION_LEN: [(u8, usize); 4] = [
    (MASK_C, COUNTER_WIRE_LEN),
    (MASK_A, AGGREGATE_WIRE_LEN),
    (MASK_G, GAUGE_WIRE_LEN),
    (MASK_T, TRACE_WIRE_LEN),
];

pub fn payload_len_for_mask(mask: u8) -> usize {
    SECTION_LEN
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, n)| n)
        .sum()
}

pub fn mask_for_len(len: usize) -> Option<u8> {
    for mask in 0u8..=0x0f {
        if payload_len_for_mask(mask) == len {
            return Some(mask);
        }
    }
    None
}

pub fn lengths_unique() -> bool {
    let mut seen = BTreeMap::new();
    for mask in 0u8..=0x0f {
        let n = payload_len_for_mask(mask);
        if seen.insert(n, mask).is_some() {
            return false;
        }
    }
    true
}

pub const COUNTER_NAMES: [&str; 46] = [
    "rx_packets",
    "xdp_invocations",
    "xdp_pass",
    "xdp_drop",
    "xdp_tx",
    "xdp_aborted",
    "rx_delivered",
    "rx_discarded",
    "rx_queue_overflow",
    "tx_submitted",
    "tx_failed",
    "tx_completed",
    "runtime_errors",
    "tx_fail_bad_buf",
    "tx_fail_token",
    "tx_grp_src",
    "tx_enq",
    "tx_enq_fail_claim",
    "tx_enq_fail_ring",
    "tx_kick",
    "tx_poll",
    "tx_cap_wait",
    "tx_nomem_wait",
    "tx_no_retain",
    "tx_flush",
    "tx_air_done_fail",
    "tx_ns_ref",
    "tx_ns_free",
    "tx_flow_sample",
    "tx_block_gt10ms",
    "tx_block_gt100ms",
    "inactive_skip",
    "reentry",
    "stack_misalign",
    "bad_md",
    "helper_unresolved",
    "helper_map",
    "pool_drop_misalign",
    "tx_wifi_last_rc",
    "tx_pend",
    "tx_inflight",
    "tx_inflight_cap",
    "tx_pending_peak",
    "tx_inflight_peak",
    "tx_block_max_us",
    "block_since_us",
];

pub const AGG_NAMES: [&str; 8] = [
    "rx_processing",
    "xdp_execution",
    "xdp_dispatch",
    "post_action",
    "tx_submission",
    "tx_block",
    "helper_execution",
    "map_operation",
];

pub const GAUGE_NAMES: [&str; 4] = [
    "rx_queue_depth",
    "tx_queue_depth",
    "tx_inflight",
    "trace_buffer_usage",
];

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub mask: u8,
    pub counters: BTreeMap<String, i64>,
    pub aggregates: Vec<AggSnap>,
    pub gauges: BTreeMap<String, (i64, i64)>,
    pub trace: Option<TraceSnap>,
}

#[derive(Clone, Debug)]
pub struct AggSnap {
    pub name: String,
    pub count: u64,
    pub total_ticks: u64,
    pub min_ticks: u64,
    pub max_ticks: u64,
    pub lo: u64,
    pub width: u64,
    pub buckets: [u64; 16],
}

#[derive(Clone, Debug)]
pub struct TraceEvent {
    pub kind: u8,
    pub arg0: u32,
    pub arg1: u32,
    pub t_us: u32,
}

#[derive(Clone, Debug)]
pub struct TraceSnap {
    pub len: u32,
    pub dropped: u32,
    pub app_len: u32,
    pub app_dropped: u32,
    pub events: Vec<TraceEvent>,
}

pub fn decode(payload: &[u8]) -> Result<Report> {
    let mask = mask_for_len(payload.len()).context("unknown get-perf payload length")?;
    let mut off = 0usize;
    let mut report = Report {
        mask,
        ..Report::default()
    };
    if mask & MASK_C != 0 {
        let end = off + COUNTER_WIRE_LEN;
        report.counters = decode_counters(&payload[off..end])?;
        off = end;
    }
    if mask & MASK_A != 0 {
        let end = off + AGGREGATE_WIRE_LEN;
        report.aggregates = decode_aggregates(&payload[off..end])?;
        off = end;
    }
    if mask & MASK_G != 0 {
        let end = off + GAUGE_WIRE_LEN;
        report.gauges = decode_gauges(&payload[off..end])?;
        off = end;
    }
    if mask & MASK_T != 0 {
        let end = off + TRACE_WIRE_LEN;
        report.trace = Some(decode_trace(&payload[off..end])?);
        off = end;
    }
    if off != payload.len() {
        bail!("get-perf decode leftover {} bytes", payload.len() - off);
    }
    Ok(report)
}

fn u32_le(buf: &[u8]) -> u32 {
    u32::from_le_bytes(buf.try_into().unwrap())
}
fn i32_le(buf: &[u8]) -> i32 {
    i32::from_le_bytes(buf.try_into().unwrap())
}
fn u64_le(buf: &[u8]) -> u64 {
    u64::from_le_bytes(buf.try_into().unwrap())
}

fn decode_counters(buf: &[u8]) -> Result<BTreeMap<String, i64>> {
    if buf.len() != COUNTER_WIRE_LEN {
        bail!("counter section len");
    }
    let mut out = BTreeMap::new();
    for (i, name) in COUNTER_NAMES.iter().enumerate() {
        let off = i * 4;
        let v = if *name == "tx_wifi_last_rc" {
            i32_le(&buf[off..off + 4]) as i64
        } else {
            u32_le(&buf[off..off + 4]) as i64
        };
        out.insert((*name).to_string(), v);
    }
    Ok(out)
}

fn decode_aggregates(buf: &[u8]) -> Result<Vec<AggSnap>> {
    if buf.len() != AGGREGATE_WIRE_LEN {
        bail!("aggregate section len");
    }
    let mut out = Vec::with_capacity(8);
    for (i, name) in AGG_NAMES.iter().enumerate() {
        let off = i * 176;
        let mut o = off;
        let mut take_u64 = || {
            let v = u64_le(&buf[o..o + 8]);
            o += 8;
            v
        };
        let count = take_u64();
        let total_ticks = take_u64();
        let min_ticks = take_u64();
        let max_ticks = take_u64();
        let lo = take_u64();
        let width = take_u64();
        let mut buckets = [0u64; 16];
        for b in &mut buckets {
            *b = take_u64();
        }
        out.push(AggSnap {
            name: (*name).to_string(),
            count,
            total_ticks,
            min_ticks,
            max_ticks,
            lo,
            width,
            buckets,
        });
    }
    Ok(out)
}

fn decode_gauges(buf: &[u8]) -> Result<BTreeMap<String, (i64, i64)>> {
    if buf.len() != GAUGE_WIRE_LEN {
        bail!("gauge section len");
    }
    let mut out = BTreeMap::new();
    for (i, name) in GAUGE_NAMES.iter().enumerate() {
        let off = i * 8;
        let current = u32_le(&buf[off..off + 4]) as i64;
        let peak = u32_le(&buf[off + 4..off + 8]) as i64;
        out.insert((*name).to_string(), (current, peak));
    }
    Ok(out)
}

fn decode_trace(buf: &[u8]) -> Result<TraceSnap> {
    if buf.len() != TRACE_WIRE_LEN {
        bail!("trace section len");
    }
    let len = u32_le(&buf[0..4]);
    let dropped = u32_le(&buf[4..8]);
    let app_len = u32_le(&buf[8..12]);
    let app_dropped = u32_le(&buf[12..16]);
    let nr = (len as usize).min(TRACE_CAP);
    let na = (app_len as usize).min(TRACE_APP_CAP);
    let mut events = Vec::with_capacity(nr + na);
    let radio_base = 16;
    for i in 0..nr {
        let off = radio_base + i * 16;
        events.push(TraceEvent {
            kind: buf[off],
            arg0: u32_le(&buf[off + 4..off + 8]),
            arg1: u32_le(&buf[off + 8..off + 12]),
            t_us: u32_le(&buf[off + 12..off + 16]),
        });
    }
    let app_base = radio_base + TRACE_CAP * 16;
    for i in 0..na {
        let off = app_base + i * 16;
        events.push(TraceEvent {
            kind: buf[off],
            arg0: u32_le(&buf[off + 4..off + 8]),
            arg1: u32_le(&buf[off + 8..off + 12]),
            t_us: u32_le(&buf[off + 12..off + 16]),
        });
    }
    Ok(TraceSnap {
        len,
        dropped,
        app_len,
        app_dropped,
        events,
    })
}
