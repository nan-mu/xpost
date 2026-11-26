# Experiment database

Protocol V1 stores new measurements in the NAS database `experiments`.
The historical SQL files in `xpost-test/sql/` document previous storage layouts.
The runner uses the schema in [protocol_v1.sql](../sql/protocol_v1.sql).

## TR3000 settings

The runner reads `/root/.db` when a selection first needs PostgreSQL.
Use a regular file with owner root and mode `0600`.
Set all five keys in the file.

```dotenv
PGHOST=<NAS address>
PGPORT=5432
PGUSER=experiment_rw
PGPASSWORD='<password>'
PGDATABASE=experiments
```

Use one assignment per line. Use a port from 1 through 65535.
The parser skips blank lines and full-line comments.
Each key can occur once. The parser rejects unknown keys and empty values.
Whitespace outside a value is optional.
Inside single quotes, every character is literal until the closing quote.
Inside double quotes, use `\"` for a quote and `\\` for a backslash.
The parser expands no variables and runs no commands.
For an unquoted value, whitespace followed by `#` starts a comment.
The parser accepts whitespace and a comment after a quoted value.

The connection uses PostgreSQL without TLS. Use the trusted experiment network.
Plan selections and `dev` do not read the file.

## Prepare a new collection

The runner connects to the database in `PGDATABASE`.
It manages Protocol V1 objects in the `release` schema.
The schema contains 12 shared `bench_` tables and 30 tables per mode.
For example, `release.bench_run` tracks attempts and `release.hal_exec_batch` holds HAL batches.

A new `all`, `medium`, or `exec_bench` run compares each managed table with [protocol_v1.sql](../sql/protocol_v1.sql).
The comparison checks column names, types, order, and nullability.
The runner creates missing tables and keeps compatible tables and their records.
**It drops each table with incompatible columns and removes that table's data before it recreates the table.**
The runner leaves unmanaged tables in place.

One transaction applies the changes.
The runner restores affected Protocol V1 foreign keys on retained tables.
The transaction rolls back if retained rows reference removed records or if a dependency prevents the change.
Resolve the reported conflict before you start another collection.

New collection takes the exclusive schema lock during preparation.
Collection and resume hold shared locks. An active run prevents schema changes by another runner.
If the schema guard disconnects, the runner stops. A write in flight still holds its connection's shared lock.

### Prepare the schema in pgAdmin

Use an authorized database administrator session. Open Query Tool on `experiments`.
On a new target, prepare an empty `release` schema for the writer.

```sql
BEGIN;
DO $$
BEGIN
    IF current_database() <> 'experiments' THEN
        RAISE EXCEPTION 'Expected database experiments';
    END IF;
END $$;
CREATE SCHEMA IF NOT EXISTS release;
GRANT USAGE, CREATE ON SCHEMA release TO experiment_rw;
COMMIT;
```

The writer owns the objects it creates. Database-level CREATE is unnecessary.
For administrator-owned objects, apply [protocol_v1.sql](../sql/protocol_v1.sql) to the empty schema.
Grant the writer `USAGE` and `CREATE` on `release` for new collection.
Grant `SELECT` and `INSERT` on its Protocol V1 tables.
Grant `UPDATE` on `release.bench_run` and `EXECUTE` on `release.bench_valid_bounds(bigint[])`.

### Read-only checks and resume

The `db-preflight` selection reports the database name, role, and permissions on `release`.
Preflight checks the expected columns and required privileges on all 132 tables.
It reports missing or incompatible tables without changing them.
Resume checks the existing schema and recovers the original records.
A reconnect acquires a shared lock for the connection and does no DDL.
Plan selections and `dev` do not access PostgreSQL.

## Check the runner

1. Set `run = ["db-preflight"]` in `xpost-test/profile.toml`.
2. Rebuild the runner for TR3000. Use the normal build and deployment script.
3. Check exit status and database identity. Expect `experiments`, `experiment_rw`, and 132 tables in `release`.
4. Use `all-preflight` for the separate deployment, capture, and database checks before a campaign.

Both preflight selections use the bare `./xpost-test` command.
Wait for maintainer approval before starting a campaign.
