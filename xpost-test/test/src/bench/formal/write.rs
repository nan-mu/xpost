//! Typed inserts into formal tables (tokio-postgres repository).

use std::time::SystemTime;

use anyhow::{Context, Result};
use tokio_postgres::Client;

use super::decode::DecodedReport;
use super::names::{Firmware, HAL_COUNTER_NAMES, map_agg_name, map_trace_kind};
use super::{ConfigId, ExamId};

#[derive(Clone, Debug)]
pub struct TxSample {
    pub t_tx: SystemTime,
    pub t_rx: Option<SystemTime>,
    pub got_rx: bool,
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
) -> Result<ConfigId> {
    let row = client
        .query_one(
            r#"
            INSERT INTO just_tx_config (
                exam_id, firmware, target_pps, window_s,
                started_at, stopped_at, sent
            ) VALUES ($1, $2::firmware, $3, $4, $5, $6, $7)
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
            ],
        )
        .await
        .context("INSERT just_tx_config")?;
    Ok(row.get(0))
}

pub async fn insert_just_tx_data(
    client: &Client,
    config_id: ConfigId,
    samples: &[TxSample],
) -> Result<()> {
    for s in samples {
        client
            .execute(
                r#"
                INSERT INTO just_tx_data (config_id, t_tx, t_rx, got_rx)
                VALUES ($1, $2, $3, $4)
                "#,
                &[&config_id, &s.t_tx, &s.t_rx, &s.got_rx],
            )
            .await
            .context("INSERT just_tx_data")?;
    }
    Ok(())
}

pub async fn insert_just_drop_config(
    client: &Client,
    exam_id: ExamId,
    firmware: Firmware,
    target_pps: f64,
    window_s: f64,
    sample_interval_ms: Option<i32>,
    started_at: SystemTime,
    stopped_at: SystemTime,
    sent: i64,
) -> Result<ConfigId> {
    let row = client
        .query_one(
            r#"
            INSERT INTO just_drop_config (
                exam_id, firmware, target_pps, window_s, sample_interval_ms,
                started_at, stopped_at, sent
            ) VALUES ($1, $2::firmware, $3, $4, $5, $6, $7, $8)
            RETURNING id
            "#,
            &[
                &exam_id,
                &firmware.as_db(),
                &target_pps,
                &window_s,
                &sample_interval_ms,
                &started_at,
                &stopped_at,
                &sent,
            ],
        )
        .await
        .context("INSERT just_drop_config")?;
    Ok(row.get(0))
}

pub async fn insert_just_drop_data(
    client: &Client,
    config_id: ConfigId,
    samples: &[(SystemTime, i64)],
) -> Result<()> {
    for (t, v) in samples {
        client
            .execute(
                r#"
                INSERT INTO just_drop_data (config_id, t, counter_value)
                VALUES ($1, $2, $3)
                "#,
                &[&config_id, t, v],
            )
            .await
            .context("INSERT just_drop_data")?;
    }
    Ok(())
}

/// Write decoded perf sections into xdp_* or hal_* (+ optional trace_event).
pub async fn write_perf(
    client: &Client,
    exam_id: ExamId,
    config_id: ConfigId,
    firmware: Firmware,
    report: &DecodedReport,
) -> Result<()> {
    if firmware.is_hal() {
        write_hal(client, exam_id, config_id, report).await?;
    } else {
        write_xdp(client, exam_id, config_id, report).await?;
    }
    if firmware.is_trace() {
        if let Some(tr) = &report.trace {
            write_trace(client, exam_id, config_id, tr).await?;
        }
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
                VALUES ($1, $2, $3::xdp_counter_name, $4)
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
                    $1, $2, $3::xdp_agg_name, $4, $5, $6, $7, $8, $9, $10::bigint[]
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
                VALUES ($1, $2, $3::xdp_gauge_name, $4, $5)
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

async fn write_hal(
    client: &Client,
    exam_id: ExamId,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<()> {
    for name in HAL_COUNTER_NAMES {
        let Some(value) = report.counters.get(*name) else {
            continue;
        };
        client
            .execute(
                r#"
                INSERT INTO hal_counter (exam_id, config_id, name, value)
                VALUES ($1, $2, $3::hal_counter_name, $4)
                ON CONFLICT (config_id, name) DO UPDATE SET value = EXCLUDED.value
                "#,
                &[&exam_id, &config_id, name, value],
            )
            .await
            .with_context(|| format!("INSERT hal_counter {name}"))?;
    }
    for agg in &report.aggregates {
        let Some(db_name) = map_agg_name(&agg.name) else {
            continue;
        };
        let buckets: Vec<i64> = agg.buckets.iter().map(|&b| b as i64).collect();
        client
            .execute(
                r#"
                INSERT INTO hal_agg (
                    exam_id, config_id, name, count, total_ticks, min_ticks, max_ticks,
                    lo, width, buckets
                ) VALUES (
                    $1, $2, $3::hal_agg_name, $4, $5, $6, $7, $8, $9, $10::bigint[]
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
            .with_context(|| format!("INSERT hal_agg {db_name}"))?;
    }
    for (name, (current, peak)) in &report.gauges {
        let label = name.clone();
        client
            .execute(
                r#"
                INSERT INTO hal_gauge (exam_id, config_id, name, current, peak)
                VALUES ($1, $2, $3::hal_gauge_name, $4, $5)
                ON CONFLICT (config_id, name) DO UPDATE SET
                    current = EXCLUDED.current, peak = EXCLUDED.peak
                "#,
                &[&exam_id, &config_id, name, current, peak],
            )
            .await
            .with_context(|| format!("INSERT hal_gauge {label}"))?;
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
                ) VALUES ($1, $2, $3, $4::perf_trace_kind, $5, $6, $7)
                ON CONFLICT (config_id, seq) DO NOTHING
                "#,
                &[&exam_id, &config_id, &seq_i, &kind, &t_us, &arg0, &arg1],
            )
            .await
            .with_context(|| format!("INSERT trace_event seq={seq}"))?;
    }
    Ok(())
}
