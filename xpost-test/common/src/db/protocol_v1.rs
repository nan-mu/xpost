//! Protocol V1 table inventory and first-use initialization.

use anyhow::{Context, Result, ensure};
use tokio_postgres::{Client, IsolationLevel};

const SQL: &str = include_str!("../../../sql/protocol_v1.sql");
// The two-integer key space is separate from the campaign session locks.
const INITIALIZATION_LOCK: i32 = 0x5850_4f53;

pub(super) fn declared_tables() -> Vec<String> {
    SQL.lines()
        .filter_map(|line| line.strip_prefix("CREATE TABLE "))
        .map(|line| {
            line.split_whitespace()
                .next()
                .expect("Protocol V1 table name")
                .to_owned()
        })
        .collect()
}

/// Initialize a dedicated empty database, or retain an existing Protocol V1 database.
pub(crate) async fn initialize_if_empty(client: &mut Client) -> Result<bool> {
    let tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await?;
    tx.query_one(
        "SELECT pg_advisory_xact_lock($1, 1)",
        &[&INITIALIZATION_LOCK],
    )
    .await
    .context("lock Protocol V1 initialization")?;

    let tables = declared_tables();
    let present: i64 = tx
        .query_one(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relkind IN ('r', 'p') AND (n.nspname || '.' || c.relname) = ANY($1)",
            &[&tables],
        )
        .await?
        .get(0);
    if present == tables.len() as i64 {
        tx.commit().await?;
        return Ok(false);
    }
    ensure!(
        present == 0,
        "Protocol V1 database is partially initialized ({present}/{} tables). Inspect it before collecting data",
        tables.len()
    );

    let target: String = tx
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    ensure!(
        target == "experiments",
        "automatic Protocol V1 initialization requires the dedicated experiments database"
    );

    let schema = tx
        .query_opt(
            "SELECT has_schema_privilege(n.oid, 'USAGE'), has_schema_privilege(n.oid, 'CREATE'),
                    EXISTS (SELECT 1 FROM pg_class c WHERE c.relnamespace = n.oid)
                    OR EXISTS (SELECT 1 FROM pg_proc p WHERE p.pronamespace = n.oid)
                    OR EXISTS (SELECT 1 FROM pg_type t WHERE t.typnamespace = n.oid)
             FROM pg_namespace n WHERE n.nspname = 'release'",
            &[],
        )
        .await?
        .context("prepare schema release in experiments before collection")?;
    ensure!(
        !schema.get::<_, bool>(2),
        "automatic Protocol V1 initialization requires an empty release schema"
    );
    ensure!(
        schema.get::<_, bool>(0) && schema.get::<_, bool>(1),
        "automatic Protocol V1 initialization requires USAGE and CREATE on schema release"
    );

    let (_, ddl) = SQL
        .split_once("\nBEGIN;\n")
        .context("find Protocol V1 transaction start")?;
    let ddl = ddl
        .trim_end()
        .strip_suffix("\nCOMMIT;")
        .context("find Protocol V1 transaction end")?;
    tx.batch_execute(ddl)
        .await
        .context("create Protocol V1 tables in release")?;
    tx.commit()
        .await
        .context("commit Protocol V1 initialization")?;
    eprintln!(
        "Initialized {} Protocol V1 tables in experiments.release",
        tables.len()
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{SQL, declared_tables};

    #[test]
    fn the_new_protocol_declares_132_release_tables() {
        let tables = declared_tables();
        assert!(tables.iter().all(|name| name.starts_with("release.")));
        assert_eq!(tables.len(), 132);
        assert_eq!(
            tables
                .iter()
                .filter(|name| name.starts_with("release.bench_"))
                .count(),
            12
        );
        for mode in ["hal", "aot", "jit", "rbpf"] {
            let prefix = format!("release.{mode}_");
            assert_eq!(
                tables
                    .iter()
                    .filter(|name| name.starts_with(&prefix))
                    .count(),
                30
            );
        }
        assert!(!SQL.lines().any(|line| line.starts_with("CREATE SCHEMA ")));
        assert!(SQL.contains("variant text NOT NULL CHECK (variant IN ('metrics','trace'))"));
        assert!(SQL.contains(
            "ALTER TABLE release.hal_case_switch ADD CHECK(switch_kind='native_policy_select' AND source_slot IS NULL AND target_slot IS NULL AND rebind_ticks IS NULL)"
        ));
        assert!(
            !SQL.lines()
                .any(|line| line.starts_with("CREATE TABLE IF NOT EXISTS")),
            "a table that already exists must abort the new protocol, not get skipped"
        );
        assert!(
            !SQL.lines().any(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("DROP ") || trimmed.starts_with("TRUNCATE ")
            }),
            "the new protocol does not delete existing data"
        );
    }
}

#[cfg(test)]
mod bootstrap_tests {
    use super::{declared_tables, initialize_if_empty};
    use crate::db::{Database, protocol};

    #[tokio::test]
    #[ignore = "requires a disposable experiments database with empty release schema in XPOST_CASE_TEST_DB_FILE"]
    async fn concurrent_first_writers_initialize_the_protocol_once() -> anyhow::Result<()> {
        let database = Database::load_fixture()?;
        let mut first = database.connect().await?;
        let mut second = database.connect().await?;
        let tables = declared_tables();
        assert!(protocol::check(&mut first, &tables).await.is_err());

        let (a, b) = tokio::try_join!(
            initialize_if_empty(&mut first),
            initialize_if_empty(&mut second)
        )?;
        assert_ne!(a, b);
        protocol::check(&mut first, &tables).await?;
        assert!(!initialize_if_empty(&mut second).await?);
        Ok(())
    }
}
