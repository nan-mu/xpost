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

A new `all`, `medium`, or `exec_bench` run initializes an empty `experiments` database.
The runner applies Protocol V1 in one transaction before it records observations.
It retains a complete Protocol V1 schema. It stops if it finds partial or historical objects.
An existing campaign resume requires its original schema and records.

The writer can prepare missing schemas if it has `CREATE` on `experiments`.
For narrower privilege, an administrator prepares the five empty schemas.
The writer then needs `USAGE` and `CREATE` on those schemas only.
The writer owns the tables that it creates.

### Prepare empty schemas in pgAdmin

Use an authorized database administrator session. Open Query Tool on `experiments`.
Inspect the target for historical or partial objects before running the SQL.
If all five schemas already contain Protocol V1, keep the existing tables and grants.
If a schema contains unexpected objects, stop and inspect it.

On an empty target, run this SQL to prepare the five schemas and grant access.

```sql
BEGIN;
DO $$
BEGIN
    IF current_database() <> 'experiments' THEN
        RAISE EXCEPTION 'Expected database experiments';
    END IF;
END $$;
CREATE SCHEMA IF NOT EXISTS bench;
CREATE SCHEMA IF NOT EXISTS hal;
CREATE SCHEMA IF NOT EXISTS aot;
CREATE SCHEMA IF NOT EXISTS jit;
CREATE SCHEMA IF NOT EXISTS rbpf;
GRANT USAGE, CREATE ON SCHEMA bench, hal, aot, jit, rbpf TO experiment_rw;
COMMIT;
```

For an administrator-owned Protocol V1 installation, apply [protocol_v1.sql](../sql/protocol_v1.sql) to the empty target.
Grant the writer `USAGE` on the five schemas.
Grant `SELECT` and `INSERT` on their tables and `UPDATE` on `bench.run`.
Grant `EXECUTE` on `bench.valid_bounds(bigint[])`.

### Read-only privileges and readiness

The `db-preflight` selection reports the database name and login role.
It reports database-level `CREATE` as `can_create_schemas`.
It lists prepared schemas that grant both `USAGE` and `CREATE`.
It checks 12 `bench` tables and 30 tables per mode across four modes.
A preflight reports missing tables without creating them.

## Check the runner

1. Set `run = ["db-preflight"]` in `xpost-test/profile.toml`.
2. Rebuild the runner for TR3000. Use the normal build and deployment script.
3. Check exit status and database identity. Expect `experiments`, `experiment_rw`, and 132 tables.
4. Use `all-preflight` for the separate deployment, capture, and database checks before a campaign.

Both preflight selections use the bare `./xpost-test` command.
Wait for maintainer approval before starting a campaign.
