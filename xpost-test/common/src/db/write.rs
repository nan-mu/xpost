//! Typed inserts into formal tables (tokio-postgres repository).

use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use tokio_postgres::Client;

use super::decode::DecodedReport;
use super::names::{Firmware, HAL_COUNTER_NAMES, map_agg_name, map_trace_kind};
use super::{ConfigId, ExamId};

/// One recorded pass of a network cell. Trace rows omit it.
#[derive(Clone, Copy, Debug)]
pub struct CellRepeat {
    pub round: i32,
    pub plan_seed: i64,
    pub hal_first: bool,
}

/// Host clocks for one paced window.
///
/// `send_elapsed_ns` is the monotonic send interval.
/// It excludes drain and export.
/// `export_elapsed_ns` counts management and export after `drain_end`.
/// `None` on a row means the collector did not record the interval.
#[derive(Clone, Copy, Debug)]
pub struct SendSpan {
    pub send_begin: SystemTime,
    pub send_end: SystemTime,
    pub drain_end: SystemTime,
    pub send_elapsed_ns: i64,
    pub export_elapsed_ns: i64,
}

impl SendSpan {
    pub fn from_elapsed(
        send_begin: SystemTime,
        send_end: SystemTime,
        drain_end: SystemTime,
        send_elapsed: Duration,
        export_elapsed: Duration,
    ) -> Self {
        Self {
            send_begin,
            send_end,
            drain_end,
            send_elapsed_ns: elapsed_ns(send_elapsed),
            export_elapsed_ns: elapsed_ns(export_elapsed),
        }
    }
}

fn elapsed_ns(span: Duration) -> i64 {
    i64::try_from(span.as_nanos()).unwrap_or(i64::MAX)
}

/// One probe send. `t_rx` is set only when both kernel stamps form an RTT.
#[derive(Clone, Debug)]
pub struct TxSample {
    pub seq: i64,
    pub t_tx: SystemTime,
    pub t_rx: Option<SystemTime>,
    pub saw_tx: bool,
    pub saw_rx: bool,
    pub pkt_rx: bool,
    pub pkt_n: i32,
    pub kernel_tx_ns: Option<i64>,
    pub kernel_rx_ns: Option<i64>,
    pub rx_stamp_n: i32,
}

pub async fn insert_exam(client: &Client, note: Option<&str>) -> Result<ExamId> {
    let row = client
        .query_one(
            "INSERT INTO exam (started_at, note) VALUES (now(), $1) RETURNING id",
            &[&note],
        )
        .await
        .context("INSERT exam")?;
    Ok(row.get(0))
}

pub async fn stop_exam(client: &Client, exam_id: ExamId) -> Result<()> {
    client
        .execute(
            "UPDATE exam SET stopped_at = now() WHERE id = $1",
            &[&exam_id],
        )
        .await
        .context("UPDATE exam.stopped_at")?;
    Ok(())
}

pub async fn insert_just_tx_config(
    client: &Client,
    exam_id: ExamId,
    firmware: Firmware,
    target_pps: f64,
    window_s: f64,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: i64,
    ok: i64,
    matched: i64,
    lookup: bool,
    stamp_drops: i64,
    pkt_drops: i64,
    pkt_matched: i64,
    repeat: Option<CellRepeat>,
    span: Option<SendSpan>,
) -> Result<ConfigId> {
    let round = repeat.map(|r| r.round);
    let plan_seed = repeat.map(|r| r.plan_seed);
    let hal_first = repeat.map(|r| r.hal_first);
    let send_begin = span.map(|s| s.send_begin);
    let send_end = span.map(|s| s.send_end);
    let drain_end = span.map(|s| s.drain_end);
    let send_elapsed_ns = span.map(|s| s.send_elapsed_ns);
    let export_elapsed_ns = span.map(|s| s.export_elapsed_ns);
    let row = client
        .query_one(
            r#"
            INSERT INTO just_tx_config (
                exam_id, firmware, target_pps, window_s,
                started_at, stopped_at, sent, ok, matched, lookup,
                stamp_drops, pkt_drops, pkt_matched,
                round, plan_seed, hal_first,
                send_begin, send_end, drain_end, send_elapsed_ns, export_elapsed_ns
            ) VALUES (
                $1, $2::text::firmware, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16,
                $17, $18, $19, $20, $21
            )
            RETURNING id
            "#,
            &[
                &exam_id,
                &firmware.as_db(),
                &target_pps,
                &window_s,
                &started_at,
                &stopped_at,
                &sent,
                &ok,
                &matched,
                &lookup,
                &stamp_drops,
                &pkt_drops,
                &pkt_matched,
                &round,
                &plan_seed,
                &hal_first,
                &send_begin,
                &send_end,
                &drain_end,
                &send_elapsed_ns,
                &export_elapsed_ns,
            ],
        )
        .await
        .context("INSERT just_tx_config")?;
    Ok(row.get(0))
}

/// Bulk-insert one row per send via `UNNEST` (one round-trip).
pub async fn insert_just_tx_data(
    client: &Client,
    config_id: ConfigId,
    samples: &[TxSample],
) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let seq: Vec<i64> = samples.iter().map(|s| s.seq).collect();
    let t_tx: Vec<SystemTime> = samples.iter().map(|s| s.t_tx).collect();
    let t_rx: Vec<Option<SystemTime>> = samples.iter().map(|s| s.t_rx).collect();
    let saw_tx: Vec<bool> = samples.iter().map(|s| s.saw_tx).collect();
    let saw_rx: Vec<bool> = samples.iter().map(|s| s.saw_rx).collect();
    let pkt_rx: Vec<bool> = samples.iter().map(|s| s.pkt_rx).collect();
    let pkt_n: Vec<i32> = samples.iter().map(|s| s.pkt_n).collect();
    let kernel_tx_ns: Vec<Option<i64>> = samples.iter().map(|s| s.kernel_tx_ns).collect();
    let kernel_rx_ns: Vec<Option<i64>> = samples.iter().map(|s| s.kernel_rx_ns).collect();
    let rx_stamp_n: Vec<i32> = samples.iter().map(|s| s.rx_stamp_n).collect();
    client
        .execute(
            r#"
            INSERT INTO just_tx_data (
                config_id, seq, t_tx, t_rx, saw_tx, saw_rx,
                pkt_rx, pkt_n, kernel_tx_ns, kernel_rx_ns, rx_stamp_n
            )
            SELECT $1, u.seq, u.t_tx, u.t_rx, u.saw_tx, u.saw_rx,
                   u.pkt_rx, u.pkt_n, u.kernel_tx_ns, u.kernel_rx_ns, u.rx_stamp_n
            FROM UNNEST(
                $2::bigint[],
                $3::timestamptz[],
                $4::timestamptz[],
                $5::boolean[],
                $6::boolean[],
                $7::boolean[],
                $8::int[],
                $9::bigint[],
                $10::bigint[],
                $11::int[]
            ) AS u(
                seq, t_tx, t_rx, saw_tx, saw_rx,
                pkt_rx, pkt_n, kernel_tx_ns, kernel_rx_ns, rx_stamp_n
            )
            "#,
            &[
                &config_id,
                &seq,
                &t_tx,
                &t_rx,
                &saw_tx,
                &saw_rx,
                &pkt_rx,
                &pkt_n,
                &kernel_tx_ns,
                &kernel_rx_ns,
                &rx_stamp_n,
            ],
        )
        .await
        .context("INSERT just_tx_data UNNEST")?;
    Ok(())
}

pub async fn insert_just_drop_config(
    client: &Client,
    exam_id: ExamId,
    firmware: Firmware,
    target_pps: f64,
    window_s: f64,
    source_seed: i64,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: i64,
    repeat: Option<CellRepeat>,
) -> Result<ConfigId> {
    let round = repeat.map(|r| r.round);
    let plan_seed = repeat.map(|r| r.plan_seed);
    let hal_first = repeat.map(|r| r.hal_first);
    let row = client
        .query_one(
            r#"
            INSERT INTO just_drop_config (
                exam_id, firmware, target_pps, window_s, source_seed,
                started_at, stopped_at, sent,
                round, plan_seed, hal_first
            ) VALUES ($1, $2::text::firmware, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            RETURNING id
            "#,
            &[
                &exam_id,
                &firmware.as_db(),
                &target_pps,
                &window_s,
                &source_seed,
                &started_at,
                &stopped_at,
                &sent,
                &round,
                &plan_seed,
                &hal_first,
            ],
        )
        .await
        .context("INSERT just_drop_config")?;
    Ok(row.get(0))
}

pub async fn insert_just_drop_data(
    client: &Client,
    config_id: ConfigId,
    slots: &[(i32, i64)],
) -> Result<()> {
    for &(slot, value) in slots {
        client
            .execute(
                r#"
                INSERT INTO just_drop_data (config_id, slot, value)
                VALUES ($1, $2, $3)
                ON CONFLICT (config_id, slot) DO UPDATE SET value = EXCLUDED.value
                "#,
                &[&config_id, &slot, &value],
            )
            .await
            .context("INSERT just_drop_data")?;
    }
    Ok(())
}

/// Which experiment owns the HAL CAG tables for this write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfExperiment {
    JustTx,
    JustDrop,
    Firewall,
    ExecBench,
}

pub async fn insert_firewall_config(
    client: &Client,
    exam_id: ExamId,
    firmware: Firmware,
    blacklist_size: i32,
    traffic: &str,
    target_pps: f64,
    window_s: f64,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: i64,
    legal_sent: Option<i64>,
    matched: Option<i64>,
    loss: Option<f64>,
    drop_sample: Option<i64>,
    repeat: Option<CellRepeat>,
    span: Option<SendSpan>,
) -> Result<ConfigId> {
    let round = repeat.map(|r| r.round);
    let plan_seed = repeat.map(|r| r.plan_seed);
    let hal_first = repeat.map(|r| r.hal_first);
    let send_begin = span.map(|s| s.send_begin);
    let send_end = span.map(|s| s.send_end);
    let drain_end = span.map(|s| s.drain_end);
    let send_elapsed_ns = span.map(|s| s.send_elapsed_ns);
    let export_elapsed_ns = span.map(|s| s.export_elapsed_ns);
    let row = client
        .query_one(
            r#"
            INSERT INTO firewall_config (
                exam_id, firmware, blacklist_size, traffic, target_pps, window_s,
                started_at, stopped_at, sent, legal_sent, matched, loss, drop_sample,
                round, plan_seed, hal_first,
                send_begin, send_end, drain_end, send_elapsed_ns, export_elapsed_ns
            ) VALUES (
                $1, $2::text::firmware, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
                $14, $15, $16, $17, $18, $19, $20, $21
            )
            RETURNING id
            "#,
            &[
                &exam_id,
                &firmware.as_db(),
                &blacklist_size,
                &traffic,
                &target_pps,
                &window_s,
                &started_at,
                &stopped_at,
                &sent,
                &legal_sent,
                &matched,
                &loss,
                &drop_sample,
                &round,
                &plan_seed,
                &hal_first,
                &send_begin,
                &send_end,
                &drain_end,
                &send_elapsed_ns,
                &export_elapsed_ns,
            ],
        )
        .await
        .context("INSERT firewall_config")?;
    Ok(row.get(0))
}

pub async fn insert_firewall_ident(
    client: &Client,
    config_id: ConfigId,
    sent_tokens: &[u8],
    recv_tokens: &[u8],
) -> Result<()> {
    client
        .execute(
            r#"
            INSERT INTO firewall_legal_ident (config_id, sent_tokens, recv_tokens)
            VALUES ($1, $2, $3)
            "#,
            &[&config_id, &sent_tokens, &recv_tokens],
        )
        .await
        .context("INSERT firewall_legal_ident")?;
    Ok(())
}

pub async fn insert_firewall_data(
    client: &Client,
    config_id: ConfigId,
    rows: &[(Ipv4Addr, i64)],
) -> Result<()> {
    for &(ip, value) in rows {
        let key = ip.to_string();
        client
            .execute(
                r#"
                INSERT INTO firewall_data (config_id, key_ip, value)
                VALUES ($1, ($2::text)::inet, $3)
                ON CONFLICT (config_id, key_ip) DO UPDATE SET value = EXCLUDED.value
                "#,
                &[&config_id, &key, &value],
            )
            .await
            .with_context(|| format!("INSERT firewall_data {key}"))?;
    }
    Ok(())
}

/// Write decoded perf sections into xdp_* or experiment HAL tables (+ optional trace).
pub async fn write_perf(
    client: &Client,
    exam_id: ExamId,
    config_id: ConfigId,
    firmware: Firmware,
    experiment: PerfExperiment,
    report: &DecodedReport,
) -> Result<()> {
    if firmware.is_hal() {
        match experiment {
            PerfExperiment::JustTx => write_just_tx_hal(client, config_id, report).await?,
            PerfExperiment::JustDrop => write_just_drop_hal(client, config_id, report).await?,
            PerfExperiment::Firewall => write_firewall_hal(client, config_id, report).await?,
            PerfExperiment::ExecBench => write_exec_bench_hal(client, config_id, report).await?,
        }
    } else {
        write_xdp(client, exam_id, config_id, report).await?;
    }
    if let Some(tr) = &report.trace {
        write_trace(client, exam_id, config_id, tr).await?;
    }
    Ok(())
}

async fn write_xdp(
    client: &Client,
    exam_id: ExamId,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<()> {
    for (name, value) in &report.counters {
        let label = name.clone();
        client
            .execute(
                r#"
                INSERT INTO xdp_counter (exam_id, config_id, name, value)
                VALUES ($1, $2, $3::text::xdp_counter_name, $4)
                ON CONFLICT (config_id, name) DO UPDATE SET value = EXCLUDED.value
                "#,
                &[&exam_id, &config_id, name, value],
            )
            .await
            .with_context(|| format!("INSERT xdp_counter {label}"))?;
    }
    for agg in &report.aggregates {
        let Some(db_name) = map_agg_name(&agg.name) else {
            continue;
        };
        let buckets: Vec<i64> = agg.buckets.iter().map(|&b| b as i64).collect();
        client
            .execute(
                r#"
                INSERT INTO xdp_agg (
                    exam_id, config_id, name, count, total_ticks, min_ticks, max_ticks,
                    lo, width, buckets
                ) VALUES (
                    $1, $2, $3::text::xdp_agg_name, $4, $5, $6, $7, $8, $9, $10::bigint[]
                )
                ON CONFLICT (config_id, name) DO UPDATE SET
                    count = EXCLUDED.count,
                    total_ticks = EXCLUDED.total_ticks,
                    min_ticks = EXCLUDED.min_ticks,
                    max_ticks = EXCLUDED.max_ticks,
                    lo = EXCLUDED.lo,
                    width = EXCLUDED.width,
                    buckets = EXCLUDED.buckets
                "#,
                &[
                    &exam_id,
                    &config_id,
                    &db_name,
                    &(agg.count as i64),
                    &(agg.total_ticks as i64),
                    &(agg.min_ticks as i64),
                    &(agg.max_ticks as i64),
                    &(agg.lo as i64),
                    &(agg.width as i64),
                    &buckets,
                ],
            )
            .await
            .with_context(|| format!("INSERT xdp_agg {db_name}"))?;
    }
    for (name, (current, peak)) in &report.gauges {
        let label = name.clone();
        client
            .execute(
                r#"
                INSERT INTO xdp_gauge (exam_id, config_id, name, current, peak)
                VALUES ($1, $2, $3::text::xdp_gauge_name, $4, $5)
                ON CONFLICT (config_id, name) DO UPDATE SET
                    current = EXCLUDED.current, peak = EXCLUDED.peak
                "#,
                &[&exam_id, &config_id, name, current, peak],
            )
            .await
            .with_context(|| format!("INSERT xdp_gauge {label}"))?;
    }
    Ok(())
}

async fn write_just_tx_hal(
    client: &Client,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<()> {
    write_hal_tables(
        client,
        config_id,
        report,
        "just_tx_hal_counter",
        "just_tx_hal_agg",
        "just_tx_hal_gauge",
    )
    .await
}

async fn write_just_drop_hal(
    client: &Client,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<()> {
    write_hal_tables(
        client,
        config_id,
        report,
        "just_drop_hal_counter",
        "just_drop_hal_agg",
        "just_drop_hal_gauge",
    )
    .await
}

async fn write_firewall_hal(
    client: &Client,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<()> {
    write_hal_tables(
        client,
        config_id,
        report,
        "firewall_hal_counter",
        "firewall_hal_agg",
        "firewall_hal_gauge",
    )
    .await
}

async fn write_exec_bench_hal(
    client: &Client,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<()> {
    write_hal_tables(
        client,
        config_id,
        report,
        "exec_bench_hal_counter",
        "exec_bench_hal_agg",
        "exec_bench_hal_gauge",
    )
    .await
}

pub async fn insert_exec_bench_config(
    client: &Client,
    exam_id: ExamId,
    firmware: &str,
    workload: &str,
    input: &[u8],
    started_at: SystemTime,
    stopped_at: SystemTime,
) -> Result<ConfigId> {
    let row = client
        .query_one(
            r#"
            INSERT INTO exec_bench_config (
                exam_id, firmware, workload, input, started_at, stopped_at
            ) VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id
            "#,
            &[
                &exam_id,
                &firmware,
                &workload,
                &input,
                &started_at,
                &stopped_at,
            ],
        )
        .await
        .context("INSERT exec_bench_config")?;
    Ok(row.get(0))
}

pub async fn insert_exec_bench_data(
    client: &Client,
    config_id: ConfigId,
    rep: i32,
    ok: bool,
    output: Option<&[u8]>,
) -> Result<()> {
    client
        .execute(
            r#"
            INSERT INTO exec_bench_data (config_id, rep, ok, output)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (config_id, rep) DO UPDATE SET
                ok = EXCLUDED.ok,
                output = EXCLUDED.output
            "#,
            &[&config_id, &rep, &ok, &output],
        )
        .await
        .context("INSERT exec_bench_data")?;
    Ok(())
}

async fn write_hal_tables(
    client: &Client,
    config_id: ConfigId,
    report: &DecodedReport,
    counter_table: &str,
    agg_table: &str,
    gauge_table: &str,
) -> Result<()> {
    for name in HAL_COUNTER_NAMES {
        let Some(value) = report.counters.get(*name) else {
            continue;
        };
        let sql = format!(
            r#"
            INSERT INTO {counter_table} (config_id, name, value)
            VALUES ($1, $2, $3)
            ON CONFLICT (config_id, name) DO UPDATE SET value = EXCLUDED.value
            "#
        );
        client
            .execute(&sql, &[&config_id, name, value])
            .await
            .with_context(|| format!("INSERT {counter_table} {name}"))?;
    }
    for agg in &report.aggregates {
        let Some(db_name) = map_agg_name(&agg.name) else {
            continue;
        };
        let buckets: Vec<i64> = agg.buckets.iter().map(|&b| b as i64).collect();
        let sql = format!(
            r#"
            INSERT INTO {agg_table} (
                config_id, name, count, total_ticks, min_ticks, max_ticks,
                lo, width, buckets
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9::bigint[]
            )
            ON CONFLICT (config_id, name) DO UPDATE SET
                count = EXCLUDED.count,
                total_ticks = EXCLUDED.total_ticks,
                min_ticks = EXCLUDED.min_ticks,
                max_ticks = EXCLUDED.max_ticks,
                lo = EXCLUDED.lo,
                width = EXCLUDED.width,
                buckets = EXCLUDED.buckets
            "#
        );
        client
            .execute(
                &sql,
                &[
                    &config_id,
                    &db_name,
                    &(agg.count as i64),
                    &(agg.total_ticks as i64),
                    &(agg.min_ticks as i64),
                    &(agg.max_ticks as i64),
                    &(agg.lo as i64),
                    &(agg.width as i64),
                    &buckets,
                ],
            )
            .await
            .with_context(|| format!("INSERT {agg_table} {db_name}"))?;
    }
    for (name, (current, peak)) in &report.gauges {
        let label = name.clone();
        let sql = format!(
            r#"
            INSERT INTO {gauge_table} (config_id, name, current, peak)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (config_id, name) DO UPDATE SET
                current = EXCLUDED.current, peak = EXCLUDED.peak
            "#
        );
        client
            .execute(&sql, &[&config_id, name, current, peak])
            .await
            .with_context(|| format!("INSERT {gauge_table} {label}"))?;
    }
    Ok(())
}

async fn write_trace(
    client: &Client,
    exam_id: ExamId,
    config_id: ConfigId,
    tr: &super::decode::TraceSnap,
) -> Result<()> {
    for (seq, ev) in tr.events.iter().enumerate() {
        let Some(kind) = map_trace_kind(ev.kind) else {
            continue;
        };
        let seq_i = seq as i32;
        let t_us = ev.t_us as i64;
        let arg0 = ev.arg0 as i64;
        let arg1 = ev.arg1 as i64;
        client
            .execute(
                r#"
                INSERT INTO trace_event (
                    exam_id, config_id, seq, kind, t_us, arg0, arg1
                ) VALUES ($1, $2, $3, $4::text::perf_trace_kind, $5, $6, $7)
                ON CONFLICT (config_id, seq) DO NOTHING
                "#,
                &[&exam_id, &config_id, &seq_i, &kind, &t_us, &arg0, &arg1],
            )
            .await
            .with_context(|| format!("INSERT trace_event seq={seq}"))?;
    }
    Ok(())
}

/// One cold-image step. `tcpdump` is the capture process text, not a file path.
pub struct CaseObs<'a> {
    pub seq: i32,
    pub xdp: &'a str,
    pub capability: &'a str,
    pub action: &'a str,
    pub expect: &'a str,
    pub map_name: &'a str,
    pub map_slot: i32,
    pub map_before: i64,
    pub map_after: i64,
    pub sent: i32,
    pub saw_reply: bool,
    pub ok: bool,
    pub tcpdump: &'a str,
}

pub async fn insert_case_obs(client: &Client, exam_id: ExamId, row: &CaseObs<'_>) -> Result<()> {
    client
        .execute(
            r#"
            INSERT INTO case_obs (
                exam_id, seq, xdp, capability, action, expect,
                map_name, map_slot, map_before, map_after,
                sent, saw_reply, ok, tcpdump
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
            "#,
            &[
                &exam_id,
                &row.seq,
                &row.xdp,
                &row.capability,
                &row.action,
                &row.expect,
                &row.map_name,
                &row.map_slot,
                &row.map_before,
                &row.map_after,
                &row.sent,
                &row.saw_reply,
                &row.ok,
                &row.tcpdump,
            ],
        )
        .await
        .context("INSERT case_obs")?;
    Ok(())
}
