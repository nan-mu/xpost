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

## First initialization in pgAdmin

1. Use an authorized database administrator session. Open Query Tool on `experiments`.
2. Run the inventory query below. Check the returned database name. Check that the writer role exists.
3. Check the user schemas and relations. An empty target has no experiment objects.
4. If Protocol V1 objects exist, compare the five schemas against a disposable database initialized with the same DDL. Use pgAdmin Schema Diff.
5. Stop if the target has historical tables, partial initialization, or a structural difference. Report the conflicting objects.
6. On an empty target, open and execute [protocol_v1.sql](../sql/protocol_v1.sql) in Query Tool once. The file uses one transaction.
7. If the five schemas already match the DDL, proceed to the grants. Check the target database before applying the grants.

```sql
SELECT current_database() AS target_database,
       EXISTS (SELECT FROM pg_roles WHERE rolname = 'experiment_rw' AND rolcanlogin) AS writer_role;

SELECT n.nspname AS schema, c.relname AS relation, c.relkind AS kind
FROM pg_namespace AS n
LEFT JOIN pg_class AS c ON c.relnamespace = n.oid
WHERE n.nspname <> 'information_schema'
  AND left(n.nspname, 3) <> 'pg_'
ORDER BY n.nspname, c.relname;
```

```sql
BEGIN;
DO $$
BEGIN
    IF current_database() <> 'experiments' THEN
        RAISE EXCEPTION 'Expected database experiments';
    END IF;
END $$;
GRANT USAGE ON SCHEMA bench, hal, aot, jit, rbpf TO experiment_rw;
GRANT SELECT, INSERT ON ALL TABLES IN SCHEMA bench, hal, aot, jit, rbpf TO experiment_rw;
GRANT UPDATE ON TABLE bench.run TO experiment_rw;
GRANT EXECUTE ON FUNCTION bench.valid_bounds(bigint[]) TO experiment_rw;
COMMIT;
```

The runner checks its login and table access with `db-preflight`.
This selection uses a read-only transaction for table and privilege checks.
It checks 12 `bench` tables and 30 tables per mode across four modes.
It prints the database and role even if tables or privileges are missing.

## Check the runner

1. Set `run = ["db-preflight"]` in `xpost-test/profile.toml`.
2. Rebuild the runner for TR3000. Use the normal build and deployment script.
3. Check exit status and database identity. Expect `experiments`, `experiment_rw`, and 132 tables.
4. Use `all-preflight` for the separate deployment, capture, and database checks before a campaign.

Both preflight selections use the bare `./xpost-test` command.
Wait for maintainer approval before starting a campaign.
