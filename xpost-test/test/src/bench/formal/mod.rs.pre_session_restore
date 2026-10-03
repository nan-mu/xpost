//! Formal schema (`sql/formal.sql` + experiment SQL): exam-scoped tables.
//!
//! Open-run path never DROP/TRUNCATE; explicit [`clear`] empties via
//! `TRUNCATE exam CASCADE`. Decoded report bytes are written through this
//! repository (no `report_raw`).

mod decode;
mod names;
mod write;

use anyhow::{Context, Result};
use log::info;
use tokio_postgres::Client;

use crate::bench::pg_dsn::PgOpts;
use crate::config;

pub use decode::decode_report_msg;
pub use names::Firmware;
pub use write::{
    PerfExperiment, TxSample, insert_exam, insert_firewall_config, insert_just_drop_config,
    insert_just_drop_data, insert_just_tx_config, insert_just_tx_data, stop_exam, write_perf,
};

const FORMAL_SQL: &str = include_str!("../../../../sql/formal.sql");
const JUST_DROP_SQL: &str = include_str!("../../../../sql/just_drop.sql");
const JUST_TX_SQL: &str = include_str!("../../../../sql/just_tx.sql");
const FIREWALL_SQL: &str = include_str!("../../../../sql/firewall.sql");

/// Ensure `formal` schema + tables exist (CREATE IF NOT EXISTS / idempotent types).
/// Does **not** truncate or drop.
pub async fn ensure(client: &Client, opts: PgOpts) -> Result<()> {
    let schema = opts.schema();
    if opts.formal {
        let formal_schema = config::postgres().formal_schema.as_str();
        client
            .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {formal_schema}"))
            .await
            .context("CREATE SCHEMA formal")?;
        client
            .batch_execute(&format!("SET search_path TO {formal_schema}"))
            .await
            .context("SET search_path formal")?;
    } else {
        client
            .batch_execute("SET search_path TO public")
            .await
            .context("SET search_path public")?;
    }
    client
        .batch_execute(FORMAL_SQL)
        .await
        .with_context(|| format!("ensure formal.sql in {schema}"))?;
    client
        .batch_execute(JUST_DROP_SQL)
        .await
        .with_context(|| format!("ensure just_drop.sql in {schema}"))?;
    client
        .batch_execute(JUST_TX_SQL)
        .await
        .with_context(|| format!("ensure just_tx.sql in {schema}"))?;
    client
        .batch_execute(FIREWALL_SQL)
        .await
        .with_context(|| format!("ensure firewall.sql in {schema}"))?;
    info!("formal schema={schema}: ensured (no truncate)");
    Ok(())
}

/// Explicit wipe: `TRUNCATE exam CASCADE` (all child tables).
pub async fn clear(client: &Client, opts: PgOpts) -> Result<()> {
    ensure(client, opts).await?;
    client
        .batch_execute("TRUNCATE exam CASCADE")
        .await
        .context("TRUNCATE exam CASCADE")?;
    info!("formal-clear: truncated exam CASCADE");
    Ok(())
}

pub async fn connect_and_ensure(opts: PgOpts) -> Result<Client> {
    let client = crate::bench::pg_dsn::connect(opts).await?;
    ensure(&client, opts).await?;
    Ok(client)
}

/// Wipe via `run = ["formal-clear"]` in `config/default.toml`.
pub async fn formal_clear(opts: PgOpts) -> Result<()> {
    let client = crate::bench::pg_dsn::connect(opts).await?;
    clear(&client, opts).await?;
    Ok(())
}

pub type ExamId = uuid::Uuid;
pub type ConfigId = uuid::Uuid;
