//! Protocol V1 table inventory and first-use initialization.

use anyhow::{Context, Result, ensure};
use tokio_postgres::{Client, IsolationLevel};

const SQL: &str = include_str!("../../../sql/protocol_v1.sql");
const SCHEMA_PREFIX: &str = "CREATE SCHEMA IF NOT EXISTS ";
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

pub(super) fn declared_schemas() -> Vec<&'static str> {
    SQL.lines().filter_map(schema_statement).collect()
}

fn schema_statement(line: &str) -> Option<&str> {
    line.strip_prefix(SCHEMA_PREFIX)?.strip_suffix(';')
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

    let identity = tx
        .query_one(
            "SELECT current_database()::text, has_database_privilege(current_database(), 'CREATE')",
            &[],
        )
        .await?;
    ensure!(
        identity.get::<_, &str>(0) == "experiments",
        "automatic Protocol V1 initialization requires the dedicated experiments database"
    );

    let schemas = declared_schemas();
    let rows = tx
        .query(
            "SELECT n.nspname::text,
                EXISTS (SELECT 1 FROM pg_class c WHERE c.relnamespace = n.oid)
                   OR EXISTS (SELECT 1 FROM pg_proc p WHERE p.pronamespace = n.oid)
                   OR EXISTS (SELECT 1 FROM pg_type t WHERE t.typnamespace = n.oid),
                has_schema_privilege(n.oid, 'USAGE'),
                has_schema_privilege(n.oid, 'CREATE')
         FROM pg_namespace n
         WHERE n.nspname <> 'information_schema' AND left(n.nspname, 3) <> 'pg_'
         ORDER BY n.nspname",
            &[],
        )
        .await
        .context("inspect the dedicated database before initialization")?;
    let mut prepared = Vec::new();
    for row in &rows {
        let name: &str = row.get(0);
        ensure!(
            !row.get::<_, bool>(1) && (name == "public" || schemas.contains(&name)),
            "automatic Protocol V1 initialization requires an empty database; found objects or an unexpected schema in {name}"
        );
        if name != "public" {
            ensure!(
                row.get::<_, bool>(2) && row.get::<_, bool>(3),
                "automatic Protocol V1 initialization requires USAGE and CREATE on schema {name}"
            );
            prepared.push(name);
        }
    }
    let missing = schemas
        .iter()
        .filter(|name| !prepared.contains(name))
        .copied()
        .collect::<Vec<_>>();
    ensure!(
        missing.is_empty() || identity.get::<_, bool>(1),
        "automatic Protocol V1 initialization needs CREATE on experiments or empty schemas prepared by an administrator; missing schemas: {}",
        missing.join(", ")
    );

    // Reuse the standalone SQL inside this transaction. Create missing schemas
    // without IF NOT EXISTS so a concurrent external DDL collision aborts safely.
    let (_, ddl) = SQL
        .split_once("\nBEGIN;\n")
        .context("find Protocol V1 transaction start")?;
    let ddl = ddl
        .trim_end()
        .strip_suffix("\nCOMMIT;")
        .context("find Protocol V1 transaction end")?;
    let mut statements = String::with_capacity(ddl.len());
    for line in ddl.lines() {
        if let Some(name) = schema_statement(line) {
            if missing.contains(&name) {
                statements.push_str(&format!("CREATE SCHEMA {name};\n"));
            }
        } else {
            statements.push_str(line);
            statements.push('\n');
        }
    }
    tx.batch_execute(&statements)
        .await
        .context("create Protocol V1 tables")?;
    tx.commit()
        .await
        .context("commit Protocol V1 initialization")?;
    eprintln!(
        "Initialized {} Protocol V1 tables in experiments",
        tables.len()
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::SQL;

    #[test]
    fn the_new_protocol_declares_five_schemas_and_132_tables() {
        let schemas = SQL
            .lines()
            .filter(|line| line.starts_with("CREATE SCHEMA "))
            .count();
        let tables = SQL
            .lines()
            .filter(|line| line.starts_with("CREATE TABLE "))
            .count();
        assert_eq!(schemas, 5, "bench plus hal, aot, jit, rbpf");
        assert_eq!(tables, 132);
        assert!(SQL.contains("CREATE SCHEMA IF NOT EXISTS jit;"));
        assert!(SQL.contains("variant text NOT NULL CHECK (variant IN ('metrics','trace'))"));
        assert!(SQL.contains(
            "ALTER TABLE hal.case_switch ADD CHECK(switch_kind='native_policy_select' AND source_slot IS NULL AND target_slot IS NULL AND rebind_ticks IS NULL)"
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
    #[ignore = "requires an empty disposable experiments database in XPOST_CASE_TEST_DB_FILE"]
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
