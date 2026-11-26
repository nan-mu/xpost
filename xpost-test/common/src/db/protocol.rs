//! Connections and read-only checks for the dedicated experiment database.

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, Transaction};
use uuid::Uuid;

use super::{Database, protocol_v1::declared_tables};

pub async fn check_database() -> Result<()> {
    let mut client = Database::load()?.connect().await?;
    let tables = declared_tables();
    let identity = client
        .query_one(
            "SELECT current_database()::text, current_user::text,
                    has_schema_privilege(to_regnamespace('release'), 'USAGE'),
                    has_schema_privilege(to_regnamespace('release'), 'CREATE')",
            &[],
        )
        .await
        .context("read protocol database identity and release privileges")?;
    println!(
        "{}",
        serde_json::json!({
            "database": identity.get::<_, &str>(0),
            "role": identity.get::<_, &str>(1),
            "expected_tables": tables.len(),
            "release_usage": identity.get::<_, Option<bool>>(2),
            "release_create": identity.get::<_, Option<bool>>(3),
        })
    );
    check(&mut client, &tables).await?;
    println!("Protocol V1 tables and privileges verified");
    Ok(())
}

pub async fn run_status(database: &Database, run: Uuid) -> String {
    let result: Result<Option<String>> = async {
        let client = database.connect().await?;
        Ok(client
            .query_opt(
                "SELECT status FROM release.bench_run WHERE run_id=$1",
                &[&run],
            )
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
            "SELECT names.name, c.oid IS NOT NULL AS present, \
                    has_schema_privilege(n.oid, 'USAGE') AS can_use, \
                    has_table_privilege(c.oid, 'SELECT') AS can_select, \
                    has_table_privilege(c.oid, 'INSERT') AS can_insert, \
                    names.name <> 'release.bench_run' OR has_table_privilege(c.oid, 'UPDATE') AS can_update \
             FROM unnest($1::text[]) AS names(name) \
             LEFT JOIN pg_namespace n ON n.nspname = split_part(names.name, '.', 1) \
             LEFT JOIN pg_class c ON c.relnamespace = n.oid \
                 AND c.relname = split_part(names.name, '.', 2) AND c.relkind IN ('r', 'p') \
             ORDER BY names.name",
            &[&tables],
        )
        .await
        .context("check protocol database tables and privileges")?;
    let mut missing = Vec::new();
    for row in rows {
        let name = row.get::<_, &str>(0);
        if !row.get::<_, bool>(1) {
            missing.push(format!("{name} (table missing)"));
            continue;
        }
        let privileges = ["USAGE", "SELECT", "INSERT", "UPDATE"];
        let absent = privileges
            .into_iter()
            .enumerate()
            .filter_map(|(i, name)| (!row.get::<_, bool>(i + 2)).then_some(name))
            .collect::<Vec<_>>();
        if !absent.is_empty() {
            missing.push(format!("{name} ({})", absent.join(", ")));
        }
    }
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
    tx.execute("INSERT INTO release.bench_artifact(artifact_id,role,sha256,size_bytes,original_bytes,media_type) VALUES($1,$2,$3,$4,$5,'application/octet-stream')",
        &[&id,&role,&hash,&(bytes.len() as i64),&bytes]).await?;
    tx.execute(
        "INSERT INTO release.bench_run_artifact(run_id,role,artifact_id) VALUES($1,$2,$3)",
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
