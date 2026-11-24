//! Write a decoded report into one mode's experiment tables.
//!
//! One row per metric in `_data`, one row per event in `_event`. The metric name
//! comes from the wire, so this layer never needs a table of names: adding a
//! metric to a firmware plan adds rows here without a code change.
//!
//! # Why absence is written as absence
//!
//! A span with `count = 0` never ran. Writing it as a row with zeros would make
//! it indistinguishable from a metric that measured zero, and a mean computed
//! over that column would treat a missing figure as an instant one. The row is
//! still inserted, with `count = 0`, so a reader can see the metric was declared
//! and produced nothing; the queries that compute means must skip it.
//!
//! # Unused variants
//!
//! `Experiment`, `Variant`, and `DataTarget` name every experiment and every
//! table a write can reach. `exec_bench` uses them first; the network exams
//! follow. A variant with no constructor yet is part of this module's surface
//! rather than dead code, which is why the allowance sits here and names its
//! reason instead of being sprinkled per item.

#![allow(dead_code, reason = "every experiment and table shape is declared here; callers arrive as each exam is wired")]

use anyhow::{Context, Result};
use tokio_postgres::Client;

use super::decode::{DecodedReport, Kind, Metric};
use super::{ConfigId, ExamId};

/// Which experiment a report belongs to, for the `experiment` column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Experiment {
    JustTx,
    JustDrop,
    Firewall,
    ExecBench,
}

impl Experiment {
    pub const fn text(self) -> &'static str {
        match self {
            Self::JustTx => "just_tx",
            Self::JustDrop => "just_drop",
            Self::Firewall => "firewall",
            Self::ExecBench => "exec_bench",
        }
    }
}

/// Which collection produced a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Low-overhead collection over a rate grid.
    Stream,
    /// Single run with a timeline.
    Trace,
}

impl Variant {
    pub const fn text(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Trace => "trace",
        }
    }
}

/// The values one `_config` row needs that the report cannot supply.
#[derive(Clone, Debug)]
pub struct ConfigRow<'a> {
    pub experiment: Experiment,
    pub mode: &'a str,
    pub variant: Variant,
    pub round: Option<i32>,
    pub rep: Option<i32>,
    pub plan_seed: Option<i64>,
    pub mode_order: Option<&'a str>,
    pub target_pps: Option<f64>,
    pub window_s: Option<f64>,
    pub sent: i64,
    pub ok: Option<i64>,
    pub note: Option<&'a str>,
}

fn f64_to_i64(v: f64) -> Option<i64> {
    if v.is_finite() { Some(v as i64) } else { None }
}

/// Insert one `_config` row and return its id.
///
/// The table is `<mode>.<experiment>_config`, or `<mode>.exec_bench_config` for
/// exec. `started_at` and `stopped_at` come from the caller's clock: the device
/// has no wall clock, and a report cannot say when the run began.
pub async fn insert_config(
    client: &Client,
    exam_id: ExamId,
    row: &ConfigRow<'_>,
    started_at: std::time::SystemTime,
    stopped_at: Option<std::time::SystemTime>,
    send_elapsed_ns: Option<i64>,
    export_elapsed_ns: Option<i64>,
) -> Result<ConfigId> {
    super::schema::check_mode(row.mode)?;
    let table = format!("{}.{}_config", row.mode, row.experiment.text());
    let sql = format!(
        "INSERT INTO {table} (
            exam_id, experiment, mode, variant, round, rep, plan_seed, mode_order,
            target_pps, window_s, sent, ok, started_at, stopped_at,
            send_elapsed_ns, export_elapsed_ns, note
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)
         RETURNING id"
    );
    let id = client
        .query_one(
            &sql,
            &[
                &exam_id,
                &row.experiment.text(),
                &row.mode,
                &row.variant.text(),
                &row.round,
                &row.rep,
                &row.plan_seed,
                &row.mode_order,
                &row.target_pps,
                &row.window_s,
                &row.sent,
                &row.ok,
                &started_at,
                &stopped_at,
                &send_elapsed_ns,
                &export_elapsed_ns,
                &row.note,
            ],
        )
        .await
        .with_context(|| format!("INSERT {table}"))?;
    Ok(id.get(0))
}

/// Append columns an experiment's config table carries beyond the template.
///
/// Separate from [`insert_config`] because those columns differ per experiment
/// and are set after the shared row exists. A caller passes only the columns it
/// has; a missing one stays null rather than becoming zero.
pub async fn set_config_columns(
    client: &Client,
    mode: &str,
    experiment: Experiment,
    config_id: ConfigId,
    columns: &[(&str, &str)],
) -> Result<()> {
    // Column names come from the caller's own code, never from the wire, so a
    // value from a device cannot reach this identifier position.
    for (col, value) in columns {
        if !col.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            anyhow::bail!("column name {col:?} is not an identifier");
        }
        let table = format!("{}.{}_config", mode, experiment.text());
        let sql = format!("UPDATE {table} SET {col} = $2 WHERE id = $1");
        client
            .execute(&sql, &[&config_id, &value])
            .await
            .with_context(|| format!("UPDATE {table}.{col}"))?;
    }
    Ok(())
}

/// Set an experiment's bytes-valued config columns.
///
/// Separate from [`set_config_columns`] because those columns are `bytea` and a
/// text binding would be rejected by the driver. Keeping the two apart means a
/// caller cannot pass the wrong type by accident.
pub async fn set_config_bytes(
    client: &Client,
    mode: &str,
    experiment: Experiment,
    config_id: ConfigId,
    columns: &[(&str, &[u8])],
) -> Result<()> {
    for (col, value) in columns {
        if !col.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            anyhow::bail!("column name {col:?} is not an identifier");
        }
        let table = format!("{}.{}_config", mode, experiment.text());
        let sql = format!("UPDATE {table} SET {col} = $2 WHERE id = $1");
        client
            .execute(&sql, &[&config_id, &value])
            .await
            .with_context(|| format!("UPDATE {table}.{col}"))?;
    }
    Ok(())
}

/// Record an exec run's input, output, verdict, and host command time.
///
/// One statement because the three value types are different: `bytea`, `bool`,
/// and a number. Binding them together keeps a caller from pairing the wrong
/// value with a column, which a string-based setter cannot prevent.
///
/// `command_ms` is the wall time from `enable exec` to `disable exec`. It
/// includes the host round trip, so it is a record of the command, not of the
/// computation.
pub async fn set_exec_result(
    client: &Client,
    mode: &str,
    config_id: ConfigId,
    input: &[u8],
    output: &[u8],
    result_ok: bool,
    command_ms: f64,
) -> Result<()> {
    super::schema::check_mode(mode)?;
    let table = format!("{mode}.exec_bench_config");
    let sql = format!(
        "UPDATE {table}
            SET input = $2, output = $3, result_ok = $4, command_ms = $5
          WHERE id = $1"
    );
    client
        .execute(
            &sql,
            &[&config_id, &input, &output, &result_ok, &command_ms],
        )
        .await
        .with_context(|| format!("UPDATE {table} result"))?;
    Ok(())
}

/// Where a metric row goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataTarget<'a> {
    /// `<mode>.<experiment>_<variant>_data`.
    VariantFamily(&'a str, Experiment, Variant),
    /// `<mode>.exec_bench_<workload>_data`.
    ExecWorkload(&'a str, &'a str),
}

impl DataTarget<'_> {
    fn table(&self) -> String {
        match self {
            Self::VariantFamily(mode, experiment, variant) => format!(
                "{}.{}_{}_data",
                mode,
                experiment.text(),
                variant.text()
            ),
            Self::ExecWorkload(mode, workload) => {
                format!("{mode}.exec_bench_{workload}_data")
            }
        }
    }

    /// The `_event` table paired with this data table.
    fn event_table(&self) -> String {
        match self {
            Self::VariantFamily(mode, experiment, variant) => format!(
                "{}.{}_{}_event",
                mode,
                experiment.text(),
                variant.text()
            ),
            Self::ExecWorkload(mode, workload) => {
                format!("{mode}.exec_bench_{workload}_event")
            }
        }
    }
}

/// Insert one metric row.
pub async fn insert_metric(
    client: &Client,
    target: DataTarget<'_>,
    config_id: ConfigId,
    m: &Metric,
) -> Result<()> {
    if m.kind == Kind::Event {
        anyhow::bail!("an event entry is not a metric; use insert_events");
    }
    let table = target.table();
    let edges: Option<Vec<i64>> = if m.edges.is_empty() {
        None
    } else {
        Some(m.edges.iter().filter_map(|v| i64::try_from(*v).ok()).collect())
    };
    let buckets: Option<Vec<i64>> = if m.buckets.is_empty() {
        None
    } else {
        Some(
            m.buckets
                .iter()
                .filter_map(|v| i64::try_from(*v).ok())
                .collect(),
        )
    };
    let sql = format!(
        "INSERT INTO {table} (
            config_id, metric, kind, count, total_us, min_us, max_us,
            current, peak, edges, buckets
         ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (config_id, metric) DO UPDATE SET
            kind = EXCLUDED.kind, count = EXCLUDED.count,
            total_us = EXCLUDED.total_us, min_us = EXCLUDED.min_us,
            max_us = EXCLUDED.max_us, current = EXCLUDED.current,
            peak = EXCLUDED.peak, edges = EXCLUDED.edges,
            buckets = EXCLUDED.buckets"
    );
    client
        .execute(
            &sql,
            &[
                &config_id,
                &m.name,
                &m.kind.text(),
                &(m.count as i64),
                &f64_to_i64(m.total_us as f64),
                &(m.min_us as i64),
                &(m.max_us as i64),
                &(m.current as i64),
                &(m.peak as i64),
                &edges,
                &buckets,
            ],
        )
        .await
        .with_context(|| format!("INSERT {table} metric={}", m.name))?;
    Ok(())
}

/// Insert every metric of a report. Returns how many rows went in.
pub async fn insert_report_metrics(
    client: &Client,
    target: DataTarget<'_>,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<usize> {
    let mut n = 0;
    for m in &report.metrics {
        if m.kind == Kind::Event {
            continue;
        }
        insert_metric(client, target, config_id, m).await?;
        n += 1;
    }
    Ok(n)
}

/// Event kind byte to the database's `perf_trace_kind` name.
///
/// The mapping lives here because the firmware's ring contract names the kinds
/// and the database has its own spelling; an unmapped byte is skipped rather
/// than guessed at.
fn event_kind_name(kind: u8) -> Option<&'static str> {
    Some(match kind {
        1 => "rx_begin",
        2 => "xdp_begin",
        3 => "xdp_end",
        4 => "rx_end",
        5 => "slow_execution",
        6 => "rx_overflow",
        7 => "runtime_error",
        8 => "user_marker",
        9 => "span_begin",
        10 => "span_end",
        11 => "helper_enter",
        12 => "helper_leave",
        13 => "rx_delivered",
        14 => "rx_discarded",
        15 => "tx_sub_begin",
        16 => "tx_submitted",
        17 => "tx_failed",
        18 => "tx_completed",
        19 => "tx_fail_bad_buf",
        20 => "tx_fail_token",
        21 => "tx_enq",
        22 => "tx_kick",
        23 => "tx_poll",
        24 => "tx_cap_wait",
        25 => "tx_nomem_wait",
        26 => "tx_block",
        27 => "tx_enq_fail_claim",
        28 => "tx_enq_fail_ring",
        29 => "tx_flush",
        30 => "tx_grp_src",
        31 => "tx_no_retain",
        32 => "app_udp_recv",
        33 => "app_echo_begin",
        34 => "app_echo_end",
        35 => "app_udp_send_begin",
        36 => "app_udp_send_end",
        37 => "app_wifi_tx_begin",
        38 => "app_wifi_tx_end",
        _ => return None,
    })
}

/// Insert a report's events into its own `_event` table.
///
/// The table is per experiment and per mode, so one run's timeline never mixes
/// with another's. Returns the number of rows inserted and the number of events
/// whose kind had no name.
pub async fn insert_events(
    client: &Client,
    mode: &str,
    experiment: Experiment,
    variant: Variant,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<(usize, usize)> {
    insert_events_into(
        client,
        &DataTarget::VariantFamily(mode, experiment, variant),
        config_id,
        report,
    )
    .await
}

/// Insert a report's events into the `_event` table that pairs with `target`.
///
/// Exec names its tables per workload, so the event table follows the data
/// table's name rather than the experiment's: `exec_bench_<workload>_event`
/// beside `exec_bench_<workload>_data`.
pub async fn insert_events_into(
    client: &Client,
    target: &DataTarget<'_>,
    config_id: ConfigId,
    report: &DecodedReport,
) -> Result<(usize, usize)> {
    let table = target.event_table();
    let mut inserted = 0;
    let mut unknown = 0;
    for (seq, ev) in report.events.iter().enumerate() {
        let Some(kind) = event_kind_name(ev.kind) else {
            unknown += 1;
            continue;
        };
        let sql = format!(
            "INSERT INTO {table} (config_id, seq, kind, t_us, arg0, arg1)
             VALUES ($1,$2,$3::text::perf_trace_kind,$4,$5,$6)
             ON CONFLICT (config_id, seq) DO NOTHING"
        );
        client
            .execute(
                &sql,
                &[
                    &config_id,
                    &(seq as i32),
                    &kind,
                    &(ev.t_us as i64),
                    &(ev.arg0 as i64),
                    &(ev.arg1 as i64),
                ],
            )
            .await
            .with_context(|| format!("INSERT {table} seq={seq}"))?;
        inserted += 1;
    }
    Ok((inserted, unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_name_a_target_can_produce_has_an_identifier_shape() {
        // The table names go into SQL, so a name with a quote or a dot would
        // both break the statement and be a place a device value could reach.
        let targets = [
            DataTarget::VariantFamily("aot", Experiment::JustTx, Variant::Stream),
            DataTarget::VariantFamily("rbpf", Experiment::Firewall, Variant::Trace),
            DataTarget::ExecWorkload("hal", "bsort"),
        ];
        for t in targets {
            let table = t.table();
            assert!(
                table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'),
                "{table}"
            );
            assert_eq!(table.matches('.').count(), 1, "{table}");
        }
    }

    #[test]
    fn the_exec_table_is_named_for_its_workload() {
        assert_eq!(
            DataTarget::ExecWorkload("rbpf", "fletcher32").table(),
            "rbpf.exec_bench_fletcher32_data"
        );
    }

    #[test]
    fn variant_family_names_match_the_generated_schema() {
        // A mismatch here means rows land in a table the DDL never created.
        assert_eq!(
            DataTarget::VariantFamily("aot", Experiment::JustDrop, Variant::Trace).table(),
            "aot.just_drop_trace_data"
        );
        assert_eq!(
            DataTarget::VariantFamily("hal", Experiment::Firewall, Variant::Stream).table(),
            "hal.firewall_stream_data"
        );
    }

    #[test]
    fn every_ring_kind_the_firmware_defines_has_a_name() {
        // The ring contract numbers 1..=38. A gap would drop events silently.
        for kind in 1u8..=38 {
            assert!(
                event_kind_name(kind).is_some(),
                "ring kind {kind} has no database name"
            );
        }
    }

    #[test]
    fn an_unknown_ring_kind_is_skipped_not_named() {
        assert_eq!(event_kind_name(0), None);
        assert_eq!(event_kind_name(200), None);
    }
}

#[cfg(test)]
mod pg {
    //! End-to-end against a live server, so a reviewer can rerun the proof.
    //!
    //! Ignored by default: CI has no database. Run with
    //! `XPOST_TEST_PG='host=/tmp port=55434 user=t dbname=postgres' \
    //!  cargo test --release pg:: -- --ignored --nocapture`

    use super::*;
    use crate::db::{decode, schema};

    /// Bytes laid out exactly as the firmware encoder writes them.
    fn firmware_report() -> Vec<u8> {
        fn entry(b: &mut Vec<u8>, name: &str, kind: u8) {
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
            b.push(kind);
        }
        let mut b = Vec::new();
        b.extend_from_slice(&decode::MAGIC.to_le_bytes());
        b.extend_from_slice(&decode::VERSION.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.extend_from_slice(&4u16.to_le_bytes());

        entry(&mut b, "invocations", 1);
        b.extend_from_slice(&1u64.to_le_bytes());
        entry(&mut b, "xdp_pass", 1);
        b.extend_from_slice(&1u64.to_le_bytes());
        entry(&mut b, "engine_us", 2);
        for v in [5u64, 42, 7, 11] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        entry(&mut b, "flight", 5);
        b.extend_from_slice(&0u64.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        for (kind, t) in [(2u8, 100u32), (11, 101), (3, 120)] {
            b.push(kind);
            b.extend_from_slice(&[0, 0, 0]);
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(&t.to_le_bytes());
        }
        b
    }

    async fn connect() -> tokio_postgres::Client {
        let dsn = std::env::var("XPOST_TEST_PG").expect("XPOST_TEST_PG");
        let (client, conn) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
            .await
            .expect("connect");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client
    }

    async fn reset(client: &tokio_postgres::Client) {
        client
            .batch_execute(
                "DROP SCHEMA IF EXISTS aot CASCADE; DROP SCHEMA IF EXISTS rbpf CASCADE;
                 DROP SCHEMA IF EXISTS hal CASCADE; DROP SCHEMA IF EXISTS tmpl CASCADE;
                 CREATE SCHEMA IF NOT EXISTS formal;
                 CREATE TABLE IF NOT EXISTS formal.exam (
                   id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
                   started_at timestamptz NOT NULL,
                   stopped_at timestamptz, note text);",
            )
            .await
            .expect("reset");
    }

    #[tokio::test]
    #[ignore = "needs a live PostgreSQL; set XPOST_TEST_PG"]
    async fn a_report_travels_from_bytes_to_rows() {
        let client = connect().await;
        reset(&client).await;
        schema::ensure(&client).await.expect("ensure");

        let exam_id: uuid::Uuid = client
            .query_one(
                "INSERT INTO formal.exam (started_at, note) VALUES (now(), 'roundtrip') RETURNING id",
                &[],
            )
            .await
            .unwrap()
            .get(0);

        let report = decode::decode_payload(&firmware_report()).expect("decode");
        assert_eq!(report.counter("invocations"), Some(1));
        assert_eq!(report.metric("engine_us").unwrap().count, 5);
        assert_eq!(report.events.len(), 3);

        let cfg_id = insert_config(
            &client,
            exam_id,
            &ConfigRow {
                experiment: Experiment::ExecBench,
                mode: "rbpf",
                variant: Variant::Trace,
                round: Some(1),
                rep: Some(1),
                plan_seed: None,
                mode_order: None,
                target_pps: Some(1.0),
                window_s: Some(0.0),
                sent: 1,
                ok: Some(1),
                note: Some("entries=4"),
            },
            std::time::SystemTime::now(),
            Some(std::time::SystemTime::now()),
            None,
            None,
        )
        .await
        .expect("config");

        set_config_columns(
            &client,
            "rbpf",
            Experiment::ExecBench,
            cfg_id,
            &[("workload", "incr")],
        )
        .await
        .expect("columns");

        let target = DataTarget::ExecWorkload("rbpf", "incr");
        let n = insert_report_metrics(&client, target, cfg_id, &report)
            .await
            .expect("metrics");
        assert_eq!(n, 3, "three metrics; the event entry is not one");

        let (n_ev, unknown) = insert_events_into(&client, &target, cfg_id, &report)
            .await
            .expect("events");
        assert_eq!((n_ev, unknown), (3, 0));

        let row = client
            .query_one(
                "SELECT count, total_us FROM rbpf.exec_bench_incr_data
                 WHERE config_id = $1 AND metric = 'engine_us'",
                &[&cfg_id],
            )
            .await
            .expect("read back");
        let count: i64 = row.get(0);
        let total: i64 = row.get(1);
        assert_eq!((count, total), (5, 42));
        assert_eq!(total / count, 8, "the mean a reader would compute");

        let kinds: Vec<String> = client
            .query(
                "SELECT kind::text FROM rbpf.exec_bench_incr_event
                 WHERE config_id = $1 ORDER BY seq",
                &[&cfg_id],
            )
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect();
        assert_eq!(kinds, vec!["xdp_begin", "helper_enter", "xdp_end"]);

        client
            .execute("DELETE FROM formal.exam WHERE id = $1", &[&exam_id])
            .await
            .unwrap();
        let left: i64 = client
            .query_one(
                "SELECT (SELECT count(*) FROM rbpf.exec_bench_incr_data)
                      + (SELECT count(*) FROM rbpf.exec_bench_incr_event)",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(left, 0, "exam delete must cascade to data and events");
    }

    #[tokio::test]
    #[ignore = "needs a live PostgreSQL; set XPOST_TEST_PG"]
    async fn ensure_runs_twice_without_error() {
        let client = connect().await;
        reset(&client).await;
        schema::ensure(&client).await.expect("first");
        schema::ensure(&client).await.expect("second run must be safe");
    }
}
