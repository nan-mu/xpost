//! Reconcile the release schema using the canonical Protocol V1 DDL.

use super::{
    catalog,
    ddl::{Ddl, HAL_EXEC_CONFIG, workload_values},
};
use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeSet;
use tokio_postgres::{Client, IsolationLevel, Transaction};
use uuid::Uuid;

pub(super) async fn check(client: &mut Client) -> Result<()> {
    let tx = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .await?;
    tx.batch_execute("SET LOCAL search_path = pg_catalog")
        .await?;
    validate(&tx, &Ddl::load()?).await?;
    tx.commit().await?;
    Ok(())
}

async fn validate(tx: &Transaction<'_>, ddl: &Ddl) -> Result<()> {
    let names: Vec<String> = ddl.tables.keys().map(|name| (*name).into()).collect();
    let mut tables = catalog::tables(tx, &names, "").await?;
    catalog::table_objects(tx, &names, "", &mut tables).await?;
    let mut problems = Vec::new();
    for (&name, expected) in &ddl.tables {
        match tables.get(name) {
            None => problems.push(format!("release.{name}: table missing")),
            Some(table) if !table.regular => {
                problems.push(format!("release.{name}: expected a regular table"))
            }
            Some(table) if table.columns != expected.columns => problems.push(format!(
                "release.{name}: incompatible columns (expected {:?}, found {:?})",
                expected.columns, table.columns,
            )),
            Some(table) => {
                problems.extend(integrity_problems(name, table, ddl)?);
            }
        }
    }
    let permissions = tx
        .query(
            "SELECT c.relname::text,
                has_schema_privilege(n.oid, 'USAGE'),
                has_table_privilege(c.oid, 'SELECT'),
                has_table_privilege(c.oid, 'INSERT'),
                c.relname <> 'bench_run' OR has_table_privilege(c.oid, 'UPDATE')
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'release' AND c.relkind = 'r' AND c.relname::text = ANY($1)",
            &[&names],
        )
        .await?;
    for row in permissions {
        let name: &str = row.get(0);
        let missing: Vec<_> = ["USAGE", "SELECT", "INSERT", "UPDATE"]
            .into_iter()
            .enumerate()
            .filter_map(|(index, privilege)| (!row.get::<_, bool>(index + 1)).then_some(privilege))
            .collect();
        if !missing.is_empty() {
            problems.push(format!("release.{name}: missing {}", missing.join(", ")));
        }
    }
    let globals: Vec<String> = ddl
        .statements
        .iter()
        .filter(|s| s.global)
        .map(|s| s.name.to_owned())
        .collect();
    let installed = catalog::globals(tx, &globals, "").await?;
    for statement in ddl.statements.iter().filter(|s| s.global) {
        let name = statement.name;
        let Some(definitions) = installed.get(name) else {
            problems.push(format!(
                "release.{name}: Protocol V1 type or function missing"
            ));
            continue;
        };
        if let Some(domain) = statement.sql.strip_prefix("CREATE DOMAIN ") {
            let base_type = domain
                .split_once(" AS ")
                .and_then(|(_, definition)| definition.split_whitespace().next())
                .context("read Protocol V1 domain base type")?;
            let prefix = format!("domain:{base_type}:");
            if definitions.len() != 1 || !definitions[0].starts_with(&prefix) {
                problems.push(format!(
                    "release.{name}: expected domain with base type {base_type}"
                ));
            }
        }
    }
    ensure!(
        problems.is_empty(),
        "Protocol V1 check failed: {}",
        problems.join("\n")
    );
    Ok(())
}

fn integrity_problems(name: &str, table: &catalog::Table, ddl: &Ddl) -> Result<Vec<String>> {
    let expected = &ddl.tables[name];
    let mut problems = Vec::new();
    let key = format!("PRIMARY KEY ({})", expected.primary_key.join(","));
    if !expected.primary_key.is_empty() && !has_key(table, "p", &key) {
        problems.push(format!(
            "release.{name}: missing or incompatible primary key {key}"
        ));
    }
    for key in &expected.foreign_keys {
        let target = if key.target.contains('(') {
            key.target.clone()
        } else {
            let target_table = key
                .target
                .strip_prefix("release.")
                .context("read foreign key target")?;
            let primary_key = &ddl
                .tables
                .get(target_table)
                .context("find foreign key target")?
                .primary_key;
            format!("{}({})", key.target, primary_key.join(","))
        };
        let definition = format!("FOREIGN KEY {} REFERENCES {target}", key.columns);
        if !has_key(table, "f", &definition) {
            problems.push(format!(
                "release.{name}: missing or incompatible foreign key {definition}"
            ));
        }
    }

    // Check for validated constraints on the required column sets. PostgreSQL
    // rewrites CHECK expressions, so this check does not compare formula text.
    let mut checks: Vec<_> = table
        .objects
        .iter()
        .filter(|object| {
            object.kind == "c"
                && !object.definition.ends_with(" [unvalidated]")
                && !object.definition.ends_with(" NOT ENFORCED")
        })
        .collect();
    for columns in &expected.checks {
        if let Some(index) = checks
            .iter()
            .position(|object| object.check_columns == *columns)
        {
            checks.swap_remove(index);
        } else {
            problems.push(format!(
                "release.{name}: missing validated CHECK on ({})",
                columns.join(", ")
            ));
        }
    }

    if name == HAL_EXEC_CONFIG
        && let Err(error) = check_hal_workloads(table, ddl)
    {
        problems.push(error.to_string());
    }
    let expected_trigger = expected.write_trigger.as_deref().map(compact_sql);
    let mut canonical_trigger_active = false;
    for trigger in table
        .objects
        .iter()
        .filter(|object| object.active_write_trigger)
    {
        let clause = trigger
            .definition
            .rsplit_once(':')
            .map(|(clause, _)| compact_sql(clause));
        if clause == expected_trigger {
            canonical_trigger_active = true;
        } else {
            problems.push(format!(
                "release.{name}: unexpected active write trigger {}",
                trigger.name
            ));
        }
    }
    if expected_trigger.is_some() && !canonical_trigger_active {
        problems.push(format!(
            "release.{name}: missing or inactive Protocol V1 write trigger"
        ));
    }
    Ok(problems)
}

fn is_workload_check(object: &catalog::Object) -> bool {
    object.kind == "c" && object.check_columns == ["workload"]
}

fn check_hal_workloads(table: &catalog::Table, ddl: &Ddl) -> Result<()> {
    let expected: BTreeSet<&str> = ddl.tables[HAL_EXEC_CONFIG]
        .workload_allowlist
        .iter()
        .map(String::as_str)
        .collect();
    let mut validated = false;
    for object in table
        .objects
        .iter()
        .filter(|object| is_workload_check(object))
    {
        let definition = &object.definition;
        if definition.ends_with(" NOT ENFORCED") {
            continue;
        }
        let unvalidated = definition.strip_suffix(" [unvalidated]");
        let actual = workload_values(unvalidated.unwrap_or(definition));
        if actual.as_ref() != Some(&expected) {
            bail!(
                "release.{HAL_EXEC_CONFIG}: incompatible workload CHECK `{}`. Preserve the table and update the workload allowlist to {:?} before collection",
                object.name,
                expected,
            );
        }
        validated |= unvalidated.is_none();
    }
    ensure!(
        validated,
        "release.{HAL_EXEC_CONFIG}: missing validated workload CHECK"
    );
    Ok(())
}

fn has_key(table: &catalog::Table, kind: &str, definition: &str) -> bool {
    let definition = compact_sql(definition);
    table
        .objects
        .iter()
        .any(|object| object.kind == kind && compact_sql(&object.definition) == definition)
}

fn compact_sql(sql: &str) -> String {
    sql.split_ascii_whitespace().collect()
}

/// Caller holds the exclusive schema session lock.
pub(super) async fn prepare_append_only(client: &mut Client) -> Result<()> {
    let ddl = Ddl::load()?;
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL search_path = pg_catalog")
        .await?;
    let schema = tx
        .query_opt(
            "SELECT has_schema_privilege(n.oid, 'USAGE'), has_schema_privilege(n.oid, 'CREATE'),
                NOT (
                EXISTS (SELECT FROM pg_class WHERE relnamespace = n.oid) OR
                EXISTS (SELECT FROM pg_proc WHERE pronamespace = n.oid) OR
                EXISTS (SELECT FROM pg_type WHERE typnamespace = n.oid)
             ) FROM pg_namespace n WHERE nspname = 'release'",
            &[],
        )
        .await?
        .context("Protocol V1 requires the release schema")?;
    ensure!(
        schema.get::<_, bool>(0),
        "Protocol V1 collection requires USAGE on schema release"
    );
    if schema.get::<_, bool>(2) {
        ensure!(
            schema.get::<_, bool>(1),
            "Protocol V1 initialization requires CREATE on schema release"
        );
        // Every ALTER in the canonical DDL follows CREATE of the same table.
        // A conflicting object created by another session aborts this transaction.
        for statement in &ddl.statements {
            apply(
                &tx,
                statement.sql,
                &format!("initialize release.{}", statement.name),
            )
            .await?;
        }
    }
    validate(&tx, &ddl).await?;
    tx.commit().await?;
    Ok(())
}

/// Caller holds the exclusive schema session lock until reconciliation commits
/// and the session has acquired its shared collection lock.
pub(super) async fn reconcile(client: &mut Client) -> Result<()> {
    let ddl = Ddl::load()?;
    let mut tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL search_path = pg_catalog")
        .await?;
    let schema = tx
        .query_opt(
            "SELECT has_schema_privilege(oid, 'USAGE'), has_schema_privilege(oid, 'CREATE')
         FROM pg_namespace WHERE nspname = 'release'",
            &[],
        )
        .await?
        .context("prepare schema release before collection")?;
    ensure!(
        schema.get::<_, bool>(0) && schema.get::<_, bool>(1),
        "Protocol V1 preparation requires USAGE and CREATE on schema release"
    );

    let names: Vec<String> = ddl.tables.keys().map(|name| (*name).into()).collect();
    let global_names: Vec<_> = ddl
        .statements
        .iter()
        .filter(|s| s.global)
        .map(|s| s.name.to_owned())
        .collect();
    let mut installed = catalog::tables(&tx, &names, "").await?;
    catalog::table_objects(&tx, &names, "", &mut installed).await?;
    let installed_globals = catalog::globals(&tx, &global_names, "").await?;
    if let Some(table) = installed.get(HAL_EXEC_CONFIG) {
        check_hal_workloads(table, &ddl)?;
    }

    // Ask PostgreSQL to describe the DDL, including its normalized constraints.
    // The reference objects exist only inside this savepoint. A name prefix
    // avoids needing CREATE privileges on the database to make another schema.
    let prefix = format!("x{}_", &Uuid::new_v4().simple().to_string()[..8]);
    let reference_sql = ddl
        .statements
        .iter()
        .map(|s| s.shadow(&prefix))
        .collect::<Vec<_>>()
        .join("\n");
    let reference = tx.transaction().await?;
    apply(
        &reference,
        &reference_sql,
        "describe the canonical Protocol V1 DDL",
    )
    .await?;
    let shadow_names: Vec<_> = names.iter().map(|name| format!("{prefix}{name}")).collect();
    let shadow_globals: Vec<_> = global_names
        .iter()
        .map(|name| format!("{prefix}{name}"))
        .collect();
    let mut expected = catalog::tables(&reference, &shadow_names, &prefix).await?;
    catalog::table_objects(&reference, &shadow_names, &prefix, &mut expected).await?;
    let expected_globals = catalog::globals(&reference, &shadow_globals, &prefix).await?;
    reference.rollback().await?;

    for (name, definitions) in &installed_globals {
        ensure!(
            expected_globals.get(name) == Some(definitions),
            "incompatible Protocol V1 type or function release.{name}"
        );
    }
    let mut rebuild = BTreeSet::new();
    for (&name, definition) in &ddl.tables {
        let columns = &definition.columns;
        ensure!(
            expected[name].columns == *columns,
            "DDL column inventory disagrees with PostgreSQL for release.{name}"
        );
        if let Some(table) = installed.get(name) {
            ensure!(
                table.regular,
                "release.{name} is not a regular Protocol V1 table"
            );
            if table.columns != *columns {
                rebuild.insert(name);
            }
        }
    }

    let mut restore = Vec::new();
    for (name, table) in &installed {
        let canonical = &expected[name].objects;
        if rebuild.contains(name.as_str()) {
            for object in &table.objects {
                let known = matches!(object.kind.as_str(), "default" | "n")
                    || canonical.iter().any(|known| {
                        known.kind == object.kind
                            && (known.definition == object.definition
                                || (object.kind != "f" && known.name == object.name))
                    });
                ensure!(
                    known,
                    "cannot rebuild release.{name}: non-managed or incompatible {} {}",
                    object.kind,
                    object.name
                );
            }
            continue;
        }
        for wanted in canonical {
            // The read-only check and preparation use the same allowlist comparison.
            if name == HAL_EXEC_CONFIG && is_workload_check(wanted) {
                continue;
            }
            let current = table.objects.iter().find(|o| same_definition(o, wanted));
            if let Some(current) = current {
                if current.kind == "f" && rebuild.contains(current.referenced.as_str()) {
                    apply(
                        &tx,
                        &format!(
                            "ALTER TABLE release.{name} DROP CONSTRAINT {}",
                            quote(&current.name)
                        ),
                        &format!(
                            "detach release.{name}.{} from release.{}",
                            current.name, current.referenced
                        ),
                    )
                    .await?;
                    restore.push((name, &current.name, &current.definition));
                }
            } else if wanted.kind == "f" && !table.objects.iter().any(|o| o.name == wanted.name) {
                restore.push((name, &wanted.name, &wanted.definition));
            } else {
                bail!(
                    "release.{name}: missing or incompatible {} {}",
                    wanted.kind,
                    wanted.name
                );
            }
        }
    }

    if !rebuild.is_empty() {
        let names = rebuild
            .iter()
            .map(|name| format!("release.{name}"))
            .collect::<Vec<_>>()
            .join(", ");
        apply(
            &tx,
            &format!("DROP TABLE {names} RESTRICT"),
            &format!("rebuild {names}"),
        )
        .await?;
    }
    let selected = ddl.statements.iter().filter(|s| {
        if s.global {
            !installed_globals.contains_key(s.name)
        } else {
            !installed.contains_key(s.name) || rebuild.contains(s.name)
        }
    });
    for statement in selected {
        apply(
            &tx,
            statement.sql,
            &format!("create Protocol V1 object for release.{}", statement.name),
        )
        .await?;
    }
    for (table, constraint, definition) in restore {
        apply(
            &tx, &format!("ALTER TABLE release.{table} ADD CONSTRAINT {} {definition}", quote(constraint)),
            &format!("restore foreign key release.{table}.{constraint}; check the retained rows and referenced table"),
        ).await?;
    }
    validate(&tx, &ddl).await?;
    tx.commit().await?;
    for table in rebuild {
        eprintln!("Rebuilt incompatible Protocol V1 table release.{table}");
    }
    Ok(())
}

fn same_definition(a: &catalog::Object, b: &catalog::Object) -> bool {
    a.kind == b.kind
        && a.definition == b.definition
        && (!matches!(a.kind.as_str(), "default" | "index") || a.name == b.name)
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn apply(tx: &Transaction<'_>, sql: &str, operation: &str) -> Result<()> {
    tx.batch_execute(sql).await.map_err(|error| {
        if let Some(cause) = error.as_db_error() {
            anyhow::anyhow!(
                "{operation}: {}{}",
                cause.message(),
                cause
                    .detail()
                    .map(|detail| format!(" ({detail})"))
                    .unwrap_or_default()
            )
        } else {
            anyhow::Error::new(error).context(operation.to_owned())
        }
    })
}

#[cfg(test)]
mod tests;
