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

## First use

A new `all`, `medium`, or `exec_bench` run initializes an empty `release` schema in `experiments`.
The runner applies Protocol V1 in one transaction before it records observations.
It retains a complete installation and refuses to initialize a partially populated `release` schema.
A campaign resume requires its original Protocol V1 records in `release`.

The schema contains 12 shared `bench_` tables.
Each mode uses its own 30 tables with the mode name as a prefix.
For example, `release.bench_run` tracks attempts and `release.hal_exec_batch` holds HAL batches.

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
Grant the writer `USAGE` on `release`.
Grant `SELECT` and `INSERT` on its Protocol V1 tables.
Grant `UPDATE` on `release.bench_run` and `EXECUTE` on `release.bench_valid_bounds(bigint[])`.

### Read-only privileges and readiness

The `db-preflight` selection reports the database name and login role.
It checks the writer's access to `release` and verifies all 132 tables.
A preflight reports missing tables without creating them.

## Check the runner

1. Set `run = ["db-preflight"]` in `xpost-test/profile.toml`.
2. Rebuild the runner for TR3000. Use the normal build and deployment script.
3. Check exit status and database identity. Expect `experiments`, `experiment_rw`, and 132 tables in `release`.
4. Use `all-preflight` for the separate deployment, capture, and database checks before a campaign.

Both preflight selections use the bare `./xpost-test` command.
Wait for maintainer approval before starting a campaign.
