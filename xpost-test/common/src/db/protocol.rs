//! Connections and read-only checks for the dedicated experiment database.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio_postgres::Transaction;
use uuid::Uuid;

use super::{Collection, Database, ddl::Ddl, protocol_v1};

pub async fn check_database() -> Result<()> {
    let mut client = Database::load()?.connect().await?;
    let expected_tables = Ddl::load()?.tables.len();
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
            "expected_tables": expected_tables,
            "release_usage": identity.get::<_, Option<bool>>(2),
            "release_create": identity.get::<_, Option<bool>>(3),
        })
    );
    protocol_v1::check(&mut client).await?;
    println!("Protocol V1 tables and privileges verified");
    Ok(())
}

pub async fn run_status(database: &Collection<'_>, run: Uuid) -> String {
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

/// Save unmodified evidence and link it to its original attempt.
pub async fn artifact(tx: &Transaction<'_>, run: Uuid, role: &str, bytes: &[u8]) -> Result<Uuid> {
    let id = create_artifact(tx, role, bytes, "application/octet-stream", None).await?;
    link_artifact(tx, run, role, id).await?;
    Ok(id)
}

pub async fn create_artifact(
    tx: &Transaction<'_>,
    role: &str,
    bytes: &[u8],
    media_type: &str,
    metadata: Option<&serde_json::Value>,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    let hash = digest(bytes);
    tx.execute(
        "INSERT INTO release.bench_artifact(artifact_id,role,sha256,size_bytes,original_bytes,media_type,metadata)
         VALUES($1,$2,$3,$4,$5,$6,COALESCE($7,'{}'::jsonb))",
        &[&id, &role, &hash, &(bytes.len() as i64), &bytes, &media_type, &metadata],
    ).await?;
    Ok(id)
}

pub async fn link_artifact(tx: &Transaction<'_>, run: Uuid, role: &str, id: Uuid) -> Result<()> {
    tx.execute(
        "INSERT INTO release.bench_run_artifact(run_id,role,artifact_id) VALUES($1,$2,$3)",
        &[&run, &role, &id],
    )
    .await?;
    Ok(())
}

pub fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
