use super::{Ddl, check};
use crate::db::Database;
use anyhow::Result;
use tokio_postgres::Client;
use uuid::Uuid;

#[test]
fn canonical_ddl_describes_132_tables() -> Result<()> {
    let ddl = Ddl::load()?;
    assert_eq!(ddl.tables.len(), 132);
    for (prefix, count) in [
        ("bench_", 12),
        ("hal_", 30),
        ("aot_", 30),
        ("jit_", 30),
        ("rbpf_", 30),
    ] {
        assert_eq!(
            ddl.tables
                .keys()
                .filter(|name| name.starts_with(prefix))
                .count(),
            count
        );
    }
    assert!(
        !ddl.statements
            .iter()
            .any(|s| s.sql.starts_with("DROP ") || s.sql.starts_with("TRUNCATE "))
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an empty release schema in a disposable database with fixture grants"]
async fn reconciles_tables_and_protects_active_collection() -> Result<()> {
    let db = Database::load_fixture()?;
    let mut raw = db.connect().await?;
    let database: String = raw
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    anyhow::ensure!(
        database == "reconcile_fixture",
        "use the disposable reconcile_fixture database"
    );
    raw.batch_execute(
        "CREATE TABLE release.non_protocol_marker(id integer);
        INSERT INTO release.non_protocol_marker VALUES (7)",
    )
    .await?;
    assert!(format!("{:#}", check(&mut raw).await.unwrap_err()).contains("table missing"));
    assert!(db.resume().await.is_err());

    let (a, b) = tokio::join!(db.prepare(), db.prepare());
    let collection = match (a, b) {
        (Ok(guard), Err(error)) | (Err(error), Ok(guard)) => {
            assert!(format!("{error:#}").contains("in use"));
            guard
        }
        (Err(error), _) => return Err(error),
        (Ok(_), Ok(_)) => panic!("both preparers acquired exclusive protection"),
    };
    let writer = collection.connect().await?;
    let campaign = Uuid::new_v4();
    collection.lock_campaign(campaign).await?;
    let resumed = db.resume().await?;
    assert!(resumed.lock_campaign(campaign).await.is_err());
    assert!(db.prepare().await.is_err());
    drop(resumed);
    drop(writer);
    drop(collection);

    let condition = Uuid::new_v4();
    raw.execute(
        "INSERT INTO release.bench_condition(condition_id,experiment,protocol_version,parameters)
                 VALUES ($1,'firewall','1','{}')",
        &[&condition],
    )
    .await?;
    let condition_oid = table_oid(&raw, "release.bench_condition").await?;
    let artifact_oid = table_oid(&raw, "release.bench_artifact").await?;
    raw.batch_execute("DROP TABLE release.bench_calibration_sample")
        .await?;
    assert!(db.resume().await.is_err());
    let prepared = db.prepare().await?;
    assert_eq!(
        table_oid(&raw, "release.bench_artifact").await?,
        artifact_oid
    );
    drop(prepared);

    let artifact = Uuid::new_v4();
    for alteration in [
        "ALTER TABLE release.bench_artifact RENAME COLUMN media_type TO old_media_type",
        "ALTER TABLE release.bench_artifact ALTER COLUMN media_type TYPE varchar(200)",
        "ALTER TABLE release.bench_artifact DROP COLUMN media_type;
         ALTER TABLE release.bench_artifact ADD COLUMN media_type text NOT NULL DEFAULT 'old'",
        "ALTER TABLE release.bench_artifact ALTER COLUMN media_type DROP NOT NULL",
    ] {
        insert_artifact(&raw, artifact).await?;
        let old_oid = table_oid(&raw, "release.bench_artifact").await?;
        raw.batch_execute(alteration).await?;
        assert!(db.resume().await.is_err());
        assert_eq!(table_oid(&raw, "release.bench_artifact").await?, old_oid);
        assert_eq!(count(&raw, "release.bench_artifact").await?, 1);
        let prepared = db.prepare().await?;
        assert_ne!(table_oid(&raw, "release.bench_artifact").await?, old_oid);
        assert_eq!(count(&raw, "release.bench_artifact").await?, 0);
        assert_eq!(
            table_oid(&raw, "release.bench_condition").await?,
            condition_oid
        );
        assert_eq!(count(&raw, "release.bench_condition").await?, 1);
        assert_eq!(count(&raw, "release.non_protocol_marker").await?, 1);
        drop(prepared);
    }

    insert_artifact(&raw, artifact).await?;
    let config = Uuid::new_v4();
    raw.execute("INSERT INTO release.hal_firewall_config(config_id,condition_id,instrumentation,profile_hash,
        target_pps,send_window_ns,drain_ns,payload_bytes,blacklist_capacity,blacklist_entries,legal_numerator,
        legal_denominator,lookup_case,map_fixture_artifact_id)
        VALUES ($1,$2,'counter_only',repeat('a',64),400,1,1,42,256,1,1,10,'mixed',$3)",
        &[&config,&condition,&artifact]).await?;
    raw.batch_execute("ALTER TABLE release.bench_artifact ALTER COLUMN media_type DROP NOT NULL")
        .await?;
    let artifact_oid = table_oid(&raw, "release.bench_artifact").await?;
    let error = db
        .prepare()
        .await
        .err()
        .expect("retained foreign key must block rebuild");
    assert!(format!("{error:#}").contains("restore foreign key release.hal_firewall_config"));
    assert_eq!(
        table_oid(&raw, "release.bench_artifact").await?,
        artifact_oid
    );
    assert_eq!(count(&raw, "release.bench_artifact").await?, 1);
    assert_eq!(count(&raw, "release.hal_firewall_config").await?, 1);
    let fks: i64 = raw
        .query_one(
            "SELECT count(*) FROM pg_constraint
        WHERE conrelid='release.hal_firewall_config'::regclass AND convalidated
        AND confrelid='release.bench_artifact'::regclass",
            &[],
        )
        .await?
        .get(0);
    assert_eq!(fks, 1);
    raw.batch_execute(
        "DELETE FROM release.hal_firewall_config;
        CREATE VIEW release.non_protocol_view AS SELECT media_type FROM release.bench_artifact",
    )
    .await?;
    let error = db
        .prepare()
        .await
        .err()
        .expect("unmanaged view must block rebuild");
    assert!(format!("{error:#}").contains("non_protocol_view"));
    raw.batch_execute("DROP VIEW release.non_protocol_view")
        .await?;
    let collection = db.prepare().await?;
    assert_eq!(count(&raw, "release.bench_artifact").await?, 0);
    // The surviving table's restored FK still rejects references to deleted rows.
    assert!(raw.execute("INSERT INTO release.hal_firewall_config(config_id,condition_id,instrumentation,profile_hash,
        target_pps,send_window_ns,drain_ns,payload_bytes,blacklist_capacity,blacklist_entries,legal_numerator,
        legal_denominator,lookup_case,map_fixture_artifact_id)
        VALUES ($1,$2,'counter_only',repeat('a',64),400,1,1,42,256,1,1,10,'mixed',$3)",
        &[&config,&condition,&artifact]).await.unwrap_err().code()
        .is_some_and(|code| *code == tokio_postgres::error::SqlState::FOREIGN_KEY_VIOLATION));

    let acl = raw
        .query_one(
            "SELECT has_table_privilege('experiment_ro','release.bench_artifact','SELECT'),
        has_table_privilege('experiment_ro','release.bench_artifact','INSERT'),
        has_table_privilege('backup_release','release.bench_artifact','SELECT')",
            &[],
        )
        .await?;
    assert_eq!(
        (
            acl.get::<_, bool>(0),
            acl.get::<_, bool>(1),
            acl.get::<_, bool>(2)
        ),
        (true, false, true)
    );

    let writer = collection.connect().await?;
    let pid: i32 = collection
        .client()
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let error = collection
        .protect(async {
            raw.query_one("SELECT pg_terminate_backend($1)", &[&pid])
                .await?;
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            writer
                .execute("INSERT INTO release.non_protocol_marker VALUES (9)", &[])
                .await?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("guard disconnected"));
    assert!(collection.connect().await.is_err());
    assert!(
        db.prepare().await.is_err(),
        "surviving writer still holds shared protection"
    );
    assert_eq!(count(&raw, "release.non_protocol_marker").await?, 1);
    drop(writer);
    drop(collection);
    db.prepare().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the disposable reconcile_fixture database"]
async fn checks_required_constraints_and_write_triggers_before_collection() -> Result<()> {
    let db = Database::load_fixture()?;
    let mut raw = db.connect().await?;
    let database: String = raw
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    anyhow::ensure!(
        database == "reconcile_fixture",
        "use the disposable fixture"
    );
    drop(db.prepare().await?);

    let artifact = Uuid::new_v4();
    let artifact_oid = table_oid(&raw, "release.bench_artifact").await?;
    raw.batch_execute(
        "CREATE INDEX fixture_media_type ON release.bench_artifact(media_type);
         ALTER TRIGGER immutable_evidence ON release.bench_artifact RENAME TO fixture_evidence",
    )
    .await?;
    check(&mut raw).await?;
    let prepared = db.prepare().await?;
    insert_artifact(&prepared.connect().await?, artifact).await?;
    drop(prepared);
    let resumed = db.resume().await?;
    let original = resumed
        .client()
        .query_one(
            "SELECT media_type FROM release.bench_artifact WHERE artifact_id=$1",
            &[&artifact],
        )
        .await?
        .get::<_, String>(0);
    drop(resumed);
    assert_eq!(original, "fixture");

    let mut unexpected = Vec::new();
    for (table, predicate, reason) in [
        (
            "bench_artifact",
            "pg_get_constraintdef(oid) LIKE '%octet_length(original_bytes)%'",
            "CHECK",
        ),
        ("bench_run_artifact", "contype = 'p'", "primary key"),
        (
            "bench_run_artifact",
            "confrelid = 'release.bench_artifact'::regclass",
            "foreign key",
        ),
    ] {
        let constraint = raw
            .query_one(
                &format!(
                    "SELECT conname::text, pg_get_constraintdef(oid)
                     FROM pg_constraint WHERE conrelid = 'release.{table}'::regclass AND {predicate}"
                ),
                &[],
            )
            .await?;
        let name: &str = constraint.get(0);
        let definition: &str = constraint.get(1);
        let alteration = format!("ALTER TABLE release.{table}");
        raw.batch_execute(&format!("{alteration} DROP CONSTRAINT {name}"))
            .await?;
        for (entry, result) in [
            ("check", check(&mut raw).await),
            ("resume", db.resume().await.map(drop)),
        ] {
            if !result.is_err_and(|error| {
                let error = format!("{error:#}");
                error.contains(table) && error.contains(reason)
            }) {
                unexpected.push(format!(
                    "{entry} did not report missing {reason} on {table}"
                ));
            }
        }
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM pg_constraint WHERE conrelid = $1::text::regclass AND conname = $2",
                &[&format!("release.{table}"), &name],
            )
            .await?
            .get::<_, i64>(0),
            0,
            "read-only checks leave the missing constraint alone"
        );
        if reason == "CHECK" {
            let error = db
                .prepare()
                .await
                .err()
                .expect("missing CHECK must block preparation");
            assert!(format!("{error:#}").contains("release.bench_artifact"));
        }
        raw.batch_execute(&format!("{alteration} ADD CONSTRAINT {name} {definition}"))
            .await?;
    }

    raw.batch_execute(
        "CREATE FUNCTION release.fixture_rewrite() RETURNS trigger LANGUAGE plpgsql
         AS $$ BEGIN NEW.media_type := 'rewritten'; RETURN NEW; END $$;
         CREATE TRIGGER fixture_rewrite BEFORE INSERT ON release.bench_artifact
         FOR EACH ROW EXECUTE FUNCTION release.fixture_rewrite()",
    )
    .await?;
    for (entry, result) in [
        ("check", check(&mut raw).await),
        ("resume", db.resume().await.map(drop)),
        ("prepare", db.prepare().await.map(drop)),
    ] {
        if !result.is_err_and(|error| {
            let error = format!("{error:#}");
            error.contains("release.bench_artifact") && error.contains("fixture_rewrite")
        }) {
            unexpected.push(format!(
                "{entry} did not report the unexpected write trigger"
            ));
        }
    }
    let row = raw
        .query_one(
            "SELECT media_type, EXISTS (
               SELECT 1 FROM pg_trigger WHERE tgrelid='release.bench_artifact'::regclass
               AND tgname='fixture_rewrite'
             ) FROM release.bench_artifact WHERE artifact_id=$1",
            &[&artifact],
        )
        .await?;
    assert_eq!(row.get::<_, String>(0), original);
    assert!(row.get::<_, bool>(1));
    assert_eq!(
        table_oid(&raw, "release.bench_artifact").await?,
        artifact_oid
    );
    raw.batch_execute("ALTER TABLE release.bench_artifact DISABLE TRIGGER fixture_rewrite")
        .await?;
    check(&mut raw).await?;
    drop(db.resume().await?);
    drop(db.prepare().await?);
    raw.batch_execute(
        "DROP TRIGGER fixture_rewrite ON release.bench_artifact;
         ALTER TRIGGER fixture_evidence ON release.bench_artifact RENAME TO immutable_evidence;
         DROP FUNCTION release.fixture_rewrite();
         DROP INDEX release.fixture_media_type",
    )
    .await?;
    assert!(unexpected.is_empty(), "{}", unexpected.join("\n"));
    Ok(())
}

async fn insert_artifact(client: &Client, id: Uuid) -> Result<()> {
    client.execute("INSERT INTO release.bench_artifact VALUES ($1,'fixture',repeat('a',64),0,''::bytea,'fixture','{}')", &[&id]).await?;
    Ok(())
}

async fn table_oid(client: &Client, name: &str) -> Result<u32> {
    Ok(client
        .query_one("SELECT $1::text::regclass::oid", &[&name])
        .await?
        .get(0))
}

async fn count(client: &Client, table: &str) -> Result<i64> {
    Ok(client
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await?
        .get(0))
}
