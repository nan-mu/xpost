//! Binary `report` payload layout (must match `xpost::ext::perf` report encode).
//!
//! Sections in order C → A → G → T → S → P; absent features omit the section.
//! Total length uniquely identifies the feature mask.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

pub const COUNTER_WIRE_LEN: usize = 184;
pub const AGGREGATE_WIRE_LEN: usize = 8 * 176;
pub const GAUGE_WIRE_LEN: usize = 32;
pub const TRACE_CAP: usize = 256;
pub const TRACE_APP_CAP: usize = 64;
/// Dual-ring T section: n_radio, dropped_radio, n_app, dropped_app, radio[CAP], app[APP_CAP].
pub const TRACE_WIRE_LEN: usize = 16 + TRACE_CAP * 16 + TRACE_APP_CAP * 16;
pub const SPAN_WIRE_LEN: usize = 80;
pub const PCHIT_WIRE_LEN: usize = 2060;

pub const MASK_C: u8 = 1 << 0;
pub const MASK_A: u8 = 1 << 1;
pub const MASK_G: u8 = 1 << 2;
pub const MASK_T: u8 = 1 << 3;
pub const MASK_S: u8 = 1 << 4;
pub const MASK_P: u8 = 1 << 5;

const SECTION_LEN: [(u8, usize); 6] = [
    (MASK_C, COUNTER_WIRE_LEN),
    (MASK_A, AGGREGATE_WIRE_LEN),
    (MASK_G, GAUGE_WIRE_LEN),
    (MASK_T, TRACE_WIRE_LEN),
    (MASK_S, SPAN_WIRE_LEN),
    (MASK_P, PCHIT_WIRE_LEN),
];

/// Payload length for a feature mask.
pub fn payload_len_for_mask(mask: u8) -> usize {
    SECTION_LEN
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, n)| n)
        .sum()
}

/// Invert length → mask; `None` if unknown.
pub fn mask_for_len(len: usize) -> Option<u8> {
    for mask in 0u8..=0x3f {
        if payload_len_for_mask(mask) == len {
            return Some(mask);
        }
    }
    None
}

/// True when all 64 lengths are unique.
pub fn lengths_unique() -> bool {
    let mut seen = BTreeMap::new();
    for mask in 0u8..=0x3f {
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

pub const SPAN_NAMES: [&str; 5] = [
    "xdp_run",
    "tx_enqueue",
    "tx_drain_one",
    "tx_drain_batch",
    "wifi_rx_dispatch",
];

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub mask: u8,
    pub counters: BTreeMap<String, i64>,
    pub aggregates: Vec<AggSnap>,
    pub gauges: BTreeMap<String, (i64, i64)>,
    pub trace: Option<TraceSnap>,
    pub spans: BTreeMap<String, (u64, u32)>,
    pub pchit: Option<PcHitSnap>,
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
    /// Radio ring then app ring (caller may re-sort by `t_us`).
    pub events: Vec<TraceEvent>,
}

#[derive(Clone, Debug)]
pub struct PcHitSnap {
    pub samples: u32,
    pub overflow: u32,
    pub slots: Vec<(u32, u32)>,
}

pub fn decode(payload: &[u8]) -> Result<Report> {
    let mask = mask_for_len(payload.len()).context("unknown report payload length")?;
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
    if mask & MASK_S != 0 {
        let end = off + SPAN_WIRE_LEN;
        report.spans = decode_spans(&payload[off..end])?;
        off = end;
    }
    if mask & MASK_P != 0 {
        let end = off + PCHIT_WIRE_LEN;
        report.pchit = Some(decode_pchit(&payload[off..end])?);
        off = end;
    }
    if off != payload.len() {
        bail!("report decode leftover {} bytes", payload.len() - off);
    }
    Ok(report)
}

/// Format Counter section as legacy `OK counters k=v …` for existing parsers.
pub fn format_counters_ok(counters: &BTreeMap<String, i64>) -> String {
    let mut parts = Vec::with_capacity(counters.len());
    // Prefer stable wire order when present.
    for name in COUNTER_NAMES {
        if let Some(v) = counters.get(name) {
            parts.push(format!("{name}={v}"));
        }
    }
    for (k, v) in counters {
        if !COUNTER_NAMES.contains(&k.as_str()) {
            parts.push(format!("{k}={v}"));
        }
    }
    // Compatibility aliases used by older parsers / notebooks.
    if let Some(v) = counters.get("rx_discarded") {
        parts.push(format!("drop={v}"));
    }
    if let Some(v) = counters.get("tx_failed") {
        parts.push(format!("tx_wifi={v}"));
    }
    if let Some(v) = counters.get("tx_fail_bad_buf") {
        parts.push(format!("tx_bad_buf={v}"));
    }
    if let Some(v) = counters.get("tx_fail_token") {
        parts.push(format!("tx_token={v}"));
    }
    if let Some(v) = counters.get("tx_wifi_last_rc") {
        parts.push(format!("tx_wifi_rc={v}"));
    }
    if let Some(v) = counters.get("tx_submitted") {
        parts.push(format!("tx_drain_ok={v}"));
    }
    if let Some(v) = counters.get("tx_enq_fail_claim") {
        parts.push(format!("tx_enq_claim={v}"));
    }
    if let Some(v) = counters.get("tx_enq_fail_ring") {
        parts.push(format!("tx_enq_ring={v}"));
    }
    if let Some(v) = counters.get("tx_completed") {
        parts.push(format!("tx_done={v}"));
    }
    if let Some(v) = counters.get("tx_air_done_fail") {
        parts.push(format!("tx_done_fail={v}"));
    }
    if let Some(v) = counters.get("tx_wifi_last_rc") {
        parts.push(format!("tx_last_rc={v}"));
    }
    format!("OK counters {}", parts.join(" "))
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

fn decode_spans(buf: &[u8]) -> Result<BTreeMap<String, (u64, u32)>> {
    if buf.len() != SPAN_WIRE_LEN {
        bail!("span section len");
    }
    let mut out = BTreeMap::new();
    for (i, name) in SPAN_NAMES.iter().enumerate() {
        let off = i * 16;
        let cycles = u64_le(&buf[off..off + 8]);
        let entries = u32_le(&buf[off + 8..off + 12]);
        out.insert((*name).to_string(), (cycles, entries));
    }
    Ok(out)
}

fn decode_pchit(buf: &[u8]) -> Result<PcHitSnap> {
    if buf.len() != PCHIT_WIRE_LEN {
        bail!("pchit section len");
    }
    let samples = u32_le(&buf[0..4]);
    let overflow = u32_le(&buf[4..8]);
    let mut slots = Vec::new();
    for i in 0..256 {
        let off = 12 + i * 8;
        let pc = u32_le(&buf[off..off + 4]);
        let hits = u32_le(&buf[off + 4..off + 8]);
        if pc != 0 && hits != 0 {
            slots.push((pc, hits));
        }
    }
    Ok(PcHitSnap {
        samples,
        overflow,
        slots,
    })
}

/// Parse `OK report <n>` header.
pub fn parse_ok_report_len(line: &str) -> Option<usize> {
    let line = line.trim();
    let rest = line.strip_prefix("OK report ")?;
    rest.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_mask_lengths_unique() {
        assert!(lengths_unique());
    }

    #[test]
    fn known_combo_lengths() {
        assert_eq!(payload_len_for_mask(MASK_C), 184);
        assert_eq!(payload_len_for_mask(MASK_G), 32);
        assert_eq!(payload_len_for_mask(MASK_C | MASK_G), 216);
        assert_eq!(payload_len_for_mask(MASK_T), 2056);
        assert_eq!(payload_len_for_mask(MASK_P), 2060);
        assert_eq!(
            payload_len_for_mask(MASK_C | MASK_A | MASK_G | MASK_T),
            184 + 1408 + 32 + 2056
        );
    }

    #[test]
    fn counter_roundtrip_zeros() {
        let buf = [0u8; COUNTER_WIRE_LEN];
        let c = decode_counters(&buf).unwrap();
        assert_eq!(c.len(), COUNTER_NAMES.len());
        let report = decode(&buf).unwrap();
        assert_eq!(report.mask, MASK_C);
    }
}
