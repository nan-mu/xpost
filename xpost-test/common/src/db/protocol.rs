//! Connections and read-only checks for the dedicated experiment database.

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, NoTls, Transaction};
use uuid::Uuid;

use crate::config::PostgresConfig;

pub async fn connect(cfg: &PostgresConfig) -> Result<Client> {
    let database = cfg
        .protocol_dbname
        .as_deref()
        .context("set [postgres].protocol_dbname")?;
    ensure!(
        !database.trim().is_empty() && database != cfg.dbname,
        "use a protocol database separate from the historical database"
    );
    let (client, connection) = tokio_postgres::Config::new()
        .host(&cfg.host)
        .port(cfg.port)
        .dbname(database)
        .user(&cfg.user)
        .password(&cfg.password)
        .connect_timeout(Duration::from_secs(10))
        .connect(NoTls)
        .await
        .context("connect protocol database")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            log::warn!("protocol database connection: {error}");
        }
    });
    Ok(client)
}

pub async fn run_status(cfg: &PostgresConfig, run: Uuid) -> String {
    let result: Result<Option<String>> = async {
        let client = connect(cfg).await?;
        Ok(client
            .query_opt("SELECT status FROM bench.run WHERE run_id=$1", &[&run])
            .await?
            .map(|row| row.get(0)))
    }
    .await;
    match result {
        Ok(Some(status)) => status,
        Ok(None) => "no_record".into(),
        Err(error) => format!("unavailable: {error:#}"),
    }
}

/// Check the tables and privileges for the selected experiments without writing rows or DDL.
pub async fn check(client: &mut Client, tables: &[String]) -> Result<()> {
    let tx = client.build_transaction().read_only(true).start().await?;
    let rows = tx
        .query(
            "SELECT names.name FROM unnest($1::text[]) AS names(name) \
         LEFT JOIN pg_class c ON c.oid = to_regclass(names.name) \
         WHERE c.oid IS NULL OR c.relkind NOT IN ('r', 'p') \
            OR NOT has_schema_privilege(c.relnamespace, 'USAGE') \
            OR NOT has_table_privilege(c.oid, 'SELECT') \
            OR NOT has_table_privilege(c.oid, 'INSERT') \
            OR (names.name = 'bench.run' AND NOT has_table_privilege(c.oid, 'UPDATE')) \
         ORDER BY names.name",
            &[&tables],
        )
        .await
        .context("check protocol database tables and privileges")?;
    let missing = rows
        .iter()
        .map(|row| row.get::<_, &str>(0))
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty(),
        "protocol tables or privileges missing: {}",
        missing.join(", ")
    );
    tx.commit().await?;
    Ok(())
}

/// Save unmodified evidence and link it to its original attempt.
pub async fn artifact(tx: &Transaction<'_>, run: Uuid, role: &str, bytes: &[u8]) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let hash = digest(bytes);
    tx.execute("INSERT INTO bench.artifact(artifact_id,role,sha256,size_bytes,original_bytes,media_type) VALUES($1,$2,$3,$4,$5,'application/octet-stream')",
        &[&id,&role,&hash,&(bytes.len() as i64),&bytes]).await?;
    tx.execute(
        "INSERT INTO bench.run_artifact(run_id,role,artifact_id) VALUES($1,$2,$3)",
        &[&run, &role, &id],
    )
    .await?;
    Ok(id)
}

pub fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
