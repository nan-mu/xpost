//! Typed inserts into formal tables (tokio-postgres repository).

use std::net::Ipv4Addr;
use std::time::SystemTime;

use anyhow::{Context, Result};
use tokio_postgres::Client;

use super::decode::DecodedReport;
use super::names::{Firmware, HAL_COUNTER_NAMES, map_agg_name, map_trace_kind};
use super::{ConfigId, ExamId};

/// Successful bounce sample (`t_rx` observed). Loss rows are not stored.
#[derive(Clone, Debug)]
pub struct TxSample {
    pub t_tx: SystemTime,
    pub t_rx: SystemTime,
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
) -> Result<ConfigId> {
    let row = client
        .query_one(
            r#"
            INSERT INTO just_tx_config (
                exam_id, firmware, target_pps, window_s,
                started_at, stopped_at, sent, ok
            ) VALUES ($1, $2::text::firmware, $3, $4, $5, $6, $7, $8)
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
            ],
        )
        .await
        .context("INSERT just_tx_config")?;
    Ok(row.get(0))
}

/// Bulk-insert successful RTT samples via `UNNEST` (one round-trip).
pub async fn insert_just_tx_data(
    client: &Client,
    config_id: ConfigId,
    samples: &[TxSample],
) -> Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let t_tx: Vec<SystemTime> = samples.iter().map(|s| s.t_tx).collect();
    let t_rx: Vec<SystemTime> = samples.iter().map(|s| s.t_rx).collect();
    client
        .execute(
            r#"
            INSERT INTO just_tx_data (config_id, t_tx, t_rx)
            SELECT $1, u.t_tx, u.t_rx
            FROM UNNEST($2::timestamptz[], $3::timestamptz[]) AS u(t_tx, t_rx)
            "#,
            &[&config_id, &t_tx, &t_rx],
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
) -> Result<ConfigId> {
    let row = client
        .query_one(
            r#"
            INSERT INTO just_drop_config (
                exam_id, firmware, target_pps, window_s, source_seed,
                started_at, stopped_at, sent
            ) VALUES ($1, $2::text::firmware, $3, $4, $5, $6, $7, $8)
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
) -> Result<ConfigId> {
    let row = client
        .query_one(
            r#"
            INSERT INTO firewall_config (
                exam_id, firmware, blacklist_size, traffic, target_pps, window_s,
                started_at, stopped_at, sent
            ) VALUES ($1, $2::text::firmware, $3, $4, $5, $6, $7, $8, $9)
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
            ],
        )
        .await
        .context("INSERT firewall_config")?;
    Ok(row.get(0))
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
                VALUES ($1, $2::inet, $3)
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
        }
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
