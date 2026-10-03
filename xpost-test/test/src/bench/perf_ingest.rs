//! Shared serial → Postgres ingest for Aggregate / Gauge / Trace snapshots.
//! Used by `just_drop` and `just_tx` formal A/G/T windows.

use anyhow::{Context, Result, bail};
use serde_json::{Number, Value as JsonValue, json};
use tokio_postgres::Client;

#[derive(Clone, Copy, Debug)]
pub enum PerfTables {
    JustDrop,
    JustTx,
}

impl PerfTables {
    fn agg(self) -> &'static str {
        match self {
            Self::JustDrop => "just_drop_agg",
            Self::JustTx => "just_tx_agg",
        }
    }

    fn gauge(self) -> &'static str {
        match self {
            Self::JustDrop => "just_drop_gauge",
            Self::JustTx => "just_tx_gauge",
        }
    }

    fn trace(self) -> &'static str {
        match self {
            Self::JustDrop => "just_drop_trace_summary",
            Self::JustTx => "just_tx_trace_summary",
        }
    }
}

pub async fn ingest_agg(
    client: &Client,
    tables: PerfTables,
    run_id: i64,
    line: &str,
) -> Result<()> {
    let body = line
        .trim()
        .strip_prefix("OK agg")
        .context("expected OK agg")?;
    let table = tables.agg();
    for seg in body.split('|').map(str::trim).filter(|s| !s.is_empty()) {
        let mut name: Option<String> = None;
        let mut count = 0i64;
        let mut total = 0i64;
        let mut min = 0i64;
        let mut max = 0i64;
        let mut lo = 0i64;
        let mut width = 0i64;
        let mut buckets: Vec<i64> = Vec::new();
        let mut parts = seg.split_whitespace();
        if let Some(n) = parts.next() {
            name = Some(n.to_string());
        }
        for part in parts {
            let Some((k, v)) = part.split_once('=') else {
                continue;
            };
            match k {
                "count" => count = v.parse().unwrap_or(0),
                "total" => total = v.parse().unwrap_or(0),
                "min" => min = v.parse().unwrap_or(0),
                "max" => max = v.parse().unwrap_or(0),
                "lo" => lo = v.parse().unwrap_or(0),
                "width" => width = v.parse().unwrap_or(0),
                "buckets" => {
                    buckets = v.split(',').filter_map(|x| x.parse().ok()).collect();
                }
                _ => {}
            }
        }
        let Some(name) = name else {
            continue;
        };
        let buckets_json = JsonValue::Array(
            buckets
                .into_iter()
                .map(|b| JsonValue::Number(Number::from(b)))
                .collect(),
        );
        let sql = format!(
            r#"
            INSERT INTO {table} (
                run_id, name, count, total_ticks, min_ticks, max_ticks, lo, width, buckets
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#
        );
        client
            .execute(
                sql.as_str(),
                &[
                    &run_id,
                    &name,
                    &count,
                    &total,
                    &min,
                    &max,
                    &lo,
                    &width,
                    &buckets_json,
                ],
            )
            .await
            .with_context(|| format!("INSERT {table}"))?;
    }
    Ok(())
}

pub async fn ingest_gauge(
    client: &Client,
    tables: PerfTables,
    run_id: i64,
    line: &str,
) -> Result<()> {
    let body = line
        .trim()
        .strip_prefix("OK gauge ")
        .context("expected OK gauge")?;
    let table = tables.gauge();
    for part in body.split_whitespace() {
        let Some((name, rest)) = part.split_once('=') else {
            continue;
        };
        let mut it = rest.split(',');
        let current: i64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let peak: i64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let sql = format!(
            r#"
            INSERT INTO {table} (run_id, name, current, peak)
            VALUES ($1, $2, $3, $4)
            "#
        );
        client
            .execute(sql.as_str(), &[&run_id, &name, &current, &peak])
            .await
            .with_context(|| format!("INSERT {table}"))?;
    }
    Ok(())
}

pub async fn ingest_trace(
    client: &Client,
    tables: PerfTables,
    run_id: i64,
    line: &str,
) -> Result<()> {
    let body = line.trim().strip_prefix("OK trace ").unwrap_or(line);
    let mut len = 0i64;
    let mut dropped = 0i64;
    for part in body.split_whitespace() {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "len" => len = v.parse().unwrap_or(0),
                "dropped" => dropped = v.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let table = tables.trace();
    let sql = format!(
        r#"
        INSERT INTO {table} (run_id, len, dropped, kind_hist)
        VALUES ($1, $2, $3, $4)
        "#
    );
    let empty = json!({});
    client
        .execute(sql.as_str(), &[&run_id, &len, &dropped, &empty])
        .await
        .with_context(|| format!("INSERT {table}"))?;
    Ok(())
}

/// Agent `request("report")` reply: `OK report <n> b64=…`.
pub fn parse_report_b64(msg: &str) -> Result<Vec<u8>> {
    let msg = msg.trim();
    let b64 = msg
        .split_whitespace()
        .find_map(|p| p.strip_prefix("b64="))
        .context("report reply missing b64=")?;
    decode_b64(b64)
}

fn decode_b64(s: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.trim_end_matches('=');
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let a = val(bytes[i]).context("b64")?;
        let b = val(bytes[i + 1]).context("b64")?;
        let c = val(bytes[i + 2]).context("b64")?;
        let d = val(bytes[i + 3]).context("b64")?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
        out.push((c << 6) | d);
        i += 4;
    }
    let rem = bytes.len() - i;
    if rem == 2 {
        let a = val(bytes[i]).context("b64")?;
        let b = val(bytes[i + 1]).context("b64")?;
        out.push((a << 2) | (b >> 4));
    } else if rem == 3 {
        let a = val(bytes[i]).context("b64")?;
        let b = val(bytes[i + 1]).context("b64")?;
        let c = val(bytes[i + 2]).context("b64")?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
    } else if rem != 0 {
        bail!("bad b64 remainder");
    }
    // Truncate padding-induced zeros: re-decode with pad awareness via original `=` count.
    Ok(out)
}

const MASK_C: u8 = 1;
const MASK_A: u8 = 2;
const MASK_G: u8 = 4;
const MASK_T: u8 = 8;
const C_LEN: usize = 184;
const A_LEN: usize = 1408;
const G_LEN: usize = 32;
const T_LEN: usize = 16 + 256 * 16 + 64 * 16;
const S_LEN: usize = 80;
const P_LEN: usize = 2060;

fn mask_for_len(len: usize) -> Option<u8> {
    for mask in 0u8..=0x3f {
        let mut n = 0usize;
        if mask & MASK_C != 0 {
            n += C_LEN;
        }
        if mask & MASK_A != 0 {
            n += A_LEN;
        }
        if mask & MASK_G != 0 {
            n += G_LEN;
        }
        if mask & MASK_T != 0 {
            n += T_LEN;
        }
        if mask & 16 != 0 {
            n += S_LEN;
        }
        if mask & 32 != 0 {
            n += P_LEN;
        }
        if n == len {
            return Some(mask);
        }
    }
    None
}

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes(b.try_into().unwrap())
}
fn u64_le(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().unwrap())
}

const AGG_NAMES: [&str; 8] = [
    "rx_processing",
    "xdp_execution",
    "xdp_dispatch",
    "post_action",
    "tx_submission",
    "tx_block",
    "helper_execution",
    "map_operation",
];
const GAUGE_NAMES: [&str; 4] = [
    "rx_queue_depth",
    "tx_queue_depth",
    "tx_inflight",
    "trace_buffer_usage",
];

/// Ingest A/G/T sections from a binary report payload (by kind letter).
pub async fn ingest_report_kind(
    client: &Client,
    tables: PerfTables,
    run_id: i64,
    kind: char,
    payload: &[u8],
) -> Result<Option<String>> {
    let mask = mask_for_len(payload.len()).context("unknown report len")?;
    let mut off = 0usize;
    if mask & MASK_C != 0 {
        off += C_LEN;
    }
    let mut agg_off = None;
    let mut gauge_off = None;
    let mut trace_off = None;
    if mask & MASK_A != 0 {
        agg_off = Some(off);
        off += A_LEN;
    }
    if mask & MASK_G != 0 {
        gauge_off = Some(off);
        off += G_LEN;
    }
    if mask & MASK_T != 0 {
        trace_off = Some(off);
    }
    let _ = off;
    match kind {
        'a' => {
            let o = agg_off.context("report missing aggregate section")?;
            let line = format_agg_ok(&payload[o..o + A_LEN]);
            ingest_agg(client, tables, run_id, &line).await?;
            Ok(None)
        }
        'g' => {
            let o = gauge_off.context("report missing gauge section")?;
            let line = format_gauge_ok(&payload[o..o + G_LEN]);
            ingest_gauge(client, tables, run_id, &line).await?;
            Ok(None)
        }
        't' => {
            let o = trace_off.context("report missing trace section")?;
            let line = format_trace_ok(&payload[o..o + T_LEN]);
            ingest_trace(client, tables, run_id, &line).await?;
            Ok(Some(line))
        }
        _ => bail!("ingest_report_kind: unsupported kind {kind}"),
    }
}

fn format_agg_ok(buf: &[u8]) -> String {
    let mut s = String::from("OK agg");
    for (i, name) in AGG_NAMES.iter().enumerate() {
        let off = i * 176;
        let mut o = off;
        let mut take = || {
            let v = u64_le(&buf[o..o + 8]);
            o += 8;
            v
        };
        let count = take();
        let total = take();
        let min = take();
        let max = take();
        let lo = take();
        let width = take();
        let mut buckets = Vec::with_capacity(16);
        for _ in 0..16 {
            buckets.push(take().to_string());
        }
        s.push_str(&format!(
            " | {name} count={count} total={total} min={min} max={max} lo={lo} width={width} buckets={}",
            buckets.join(",")
        ));
    }
    s
}

fn format_gauge_ok(buf: &[u8]) -> String {
    let mut parts = Vec::new();
    for (i, name) in GAUGE_NAMES.iter().enumerate() {
        let off = i * 8;
        let cur = u32_le(&buf[off..off + 4]);
        let peak = u32_le(&buf[off + 4..off + 8]);
        parts.push(format!("{name}={cur},{peak}"));
    }
    format!("OK gauge {}", parts.join(" "))
}

fn trace_kind_name(kind: u8) -> &'static str {
    match kind {
        1 => "RxBegin",
        2 => "XdpBegin",
        3 => "XdpEnd",
        4 => "RxEnd",
        5 => "SlowExecution",
        6 => "RxOverflow",
        7 => "RuntimeError",
        8 => "UserMarker",
        9 => "SpanBegin",
        10 => "SpanEnd",
        11 => "HelperEnter",
        12 => "HelperLeave",
        13 => "RxDelivered",
        14 => "RxDiscarded",
        15 => "TxSubBegin",
        16 => "TxSubmitted",
        17 => "TxFailed",
        18 => "TxCompleted",
        19 => "TxFailBadBuf",
        20 => "TxFailToken",
        21 => "TxEnq",
        22 => "TxKick",
        23 => "TxPoll",
        24 => "TxCapWait",
        25 => "TxNomemWait",
        26 => "TxBlock",
        27 => "TxEnqFailClaim",
        28 => "TxEnqFailRing",
        29 => "TxFlush",
        30 => "TxGrpSrc",
        31 => "TxNoRetain",
        32 => "AppUdpRecv",
        33 => "AppEchoBegin",
        34 => "AppEchoEnd",
        35 => "AppUdpSendBegin",
        36 => "AppUdpSendEnd",
        _ => "Unknown",
    }
}

fn format_trace_ok(buf: &[u8]) -> String {
    let len = u32_le(&buf[0..4]) as usize;
    let dropped = u32_le(&buf[4..8]);
    let mut s = format!("OK trace len={len} dropped={dropped} cap=256");
    let n = len.min(256);
    for i in 0..n {
        let off = 8 + i * 16;
        let kind = trace_kind_name(buf[off]);
        let arg0 = u32_le(&buf[off + 4..off + 8]);
        let arg1 = u32_le(&buf[off + 8..off + 12]);
        let t_us = u32_le(&buf[off + 12..off + 16]);
        s.push_str(&format!(" | {kind} t_us={t_us} arg0={arg0} arg1={arg1}"));
    }
    s
}
