//! Per-mode experiment schemas, generated from templates.
//!
//! Each experiment gets a `_config` table and one or more data tables. The
//! columns that every experiment shares live in templates under the `tmpl`
//! schema, and each real table is `LIKE tmpl.x INCLUDING ALL` plus its own extra
//! columns. That keeps the shared shape in one place: a column added to a
//! template reaches every table that copies it.
//!
//! # Why a spec list and not a pile of SQL files
//!
//! There are three modes and five experiment families, so the table count runs
//! into the forties. Hand-written SQL would drift: one experiment's `_data`
//! missing a column the others have is invisible until a query fails at runtime.
//! The list below is the single description of what exists, and [`ensure`] turns
//! it into DDL. A table that is not in the list does not exist.
//!
//! # What this does not do
//!
//! It never drops or truncates. Creating is idempotent; removing a table is a
//! human's decision, because a drop silently deletes the measurements in it.

use anyhow::{bail, Context, Result};
use tokio_postgres::Client;

/// The three ways the same test can run.
pub const MODES: [&str; 3] = ["aot", "rbpf", "hal"];

/// One metric table's parent, so a foreign key can be attached after creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// `_config`: one row per run.
    Config,
    /// `_data`: one row per metric.
    Metric,
    /// `_event`: one row per timeline event.
    Event,
    /// `_send`: one row per probe packet.
    Send,
    /// `_slot`: one row per map index.
    Slot,
    /// `_ip`: one row per blacklist key.
    Ip,
    /// `_ident`: one row per run's legal-token bytes.
    Ident,
}

impl Template {
    const fn name(self) -> &'static str {
        match self {
            Self::Config => "run_config",
            Self::Metric => "metric",
            Self::Event => "event",
            Self::Send => "send",
            Self::Slot => "slot",
            Self::Ip => "ip",
            Self::Ident => "ident",
        }
    }
}

/// Extra columns a table adds on top of its template.
///
/// `sql` is appended verbatim inside the `ALTER TABLE ... ADD COLUMN` list, so a
/// column with a type the template does not have needs no new template.
#[derive(Clone, Copy)]
pub struct Extra(pub &'static [(&'static str, &'static str)]);

/// Extra columns for one experiment family's config table.
pub struct FamilySpec {
    pub family: &'static str,
    pub config_extra: Extra,
    /// Whether this family has a `trace` variant (a second metric table plus an
    /// event table).
    pub has_trace: bool,
    /// Whether this family writes a detail table, and which one.
    pub detail: Option<(&'static str, Template)>,
    /// Whether this family has a second detail table (`firewall` needs both the
    /// per-key hit count and the token bytes).
    pub detail2: Option<(&'static str, Template)>,
}

const NO_EXTRA: Extra = Extra(&[]);

/// The experiment families every mode runs.
///
/// Declared once and expanded across [`MODES`], so a new mode cannot end up with
/// half the tables: forgetting a mode is not something a hand-written list
/// catches, and a missing table shows up as a silently absent series.
pub const FAMILIES: &[FamilySpec] = &[
    FamilySpec {
        family: "just_tx",
        config_extra: Extra(&[
            ("rxcnt_delta", "bigint"),
            ("max_send_gap_ms", "double precision"),
        ]),
        has_trace: true,
        detail: Some(("just_tx_send", Template::Send)),
        detail2: None,
    },
    FamilySpec {
        family: "just_drop",
        config_extra: Extra(&[("source_seed", "bigint")]),
        has_trace: true,
        detail: Some(("just_drop_slot", Template::Slot)),
        detail2: None,
    },
    FamilySpec {
        family: "firewall",
        config_extra: Extra(&[
            ("blacklist_size", "int"),
            ("traffic", "text"),
            ("legal_sent", "bigint"),
            ("matched", "bigint"),
            ("delivery_ratio", "double precision"),
            ("drop_sample", "bigint"),
        ]),
        has_trace: true,
        detail: Some(("firewall_ip", Template::Ip)),
        detail2: Some(("firewall_ident", Template::Ident)),
    },
];

/// The eight exec_bench workloads, one metric table each.
pub const WORKLOADS: [&str; 8] = [
    "incr",
    "square",
    "bitswap",
    "fib",
    "buf",
    "memcpy",
    "fletcher32",
    "bsort",
];

/// `(table, template, config parent)` triples for one mode.
///
/// This is the single expansion point: everything downstream reads it, so the
/// table set a query expects is the table set the DDL created.
pub fn expand(mode: &str) -> Vec<(String, Template, Option<String>)> {
    let mut out = Vec::new();
    for spec in FAMILIES {
        let cfg = format!("{}_config", spec.family);
        out.push((cfg.clone(), Template::Config, None));
        out.push((
            format!("{}_stream_data", spec.family),
            Template::Metric,
            Some(cfg.clone()),
        ));
        if spec.has_trace {
            out.push((
                format!("{}_trace_data", spec.family),
                Template::Metric,
                Some(cfg.clone()),
            ));
            out.push((
                format!("{}_trace_event", spec.family),
                Template::Event,
                Some(cfg.clone()),
            ));
        }
        if let Some((name, tmpl)) = spec.detail {
            out.push((name.to_string(), tmpl, Some(cfg.clone())));
        }
        if let Some((name, tmpl)) = spec.detail2 {
            out.push((name.to_string(), tmpl, Some(cfg)));
        }
    }
    // exec_bench: one config, then one metric and one event table per workload.
    out.push(("exec_bench_config".to_string(), Template::Config, None));
    for wl in WORKLOADS {
        out.push((
            format!("exec_bench_{wl}_data"),
            Template::Metric,
            Some("exec_bench_config".to_string()),
        ));
        out.push((
            format!("exec_bench_{wl}_event"),
            Template::Event,
            Some("exec_bench_config".to_string()),
        ));
    }
    // case and hot_swap run on the AOT path only: the interpreter does not
    // execute RV32IMC machine code, so the switch semantics have one mode.
    if mode == "aot" {
        out.push(("case_config".to_string(), Template::Config, None));
        out.push(("case_latency".to_string(), Template::Config, None));
        out.push(("hot_swap_config".to_string(), Template::Config, None));
    }
    let _ = mode;
    out
}

/// Statements that create the shared templates.
///
/// Kept as one string: they run in order, and a partial template set would make
/// every later `LIKE` fail with a confusing message.
pub fn template_sql() -> String {
    let mut sql = String::from(
        // The event kind enum. Created here rather than assumed: the per-mode
        // tables use it, so a database that never ran the legacy schema must
        // still get it.
        "DO $do$ BEGIN
           CREATE TYPE perf_trace_kind AS ENUM (
             'rx_begin','xdp_begin','xdp_end','rx_end','slow_execution',
             'rx_overflow','runtime_error','user_marker','span_begin','span_end',
             'helper_enter','helper_leave','rx_delivered','rx_discarded',
             'tx_sub_begin','tx_submitted','tx_failed','tx_completed',
             'tx_fail_bad_buf','tx_fail_token','tx_enq','tx_kick','tx_poll',
             'tx_cap_wait','tx_nomem_wait','tx_block','tx_enq_fail_claim',
             'tx_enq_fail_ring','tx_flush','tx_grp_src','tx_no_retain',
             'app_udp_recv','app_echo_begin','app_echo_end','app_udp_send_begin',
             'app_udp_send_end','app_wifi_tx_begin','app_wifi_tx_end');
         EXCEPTION WHEN duplicate_object THEN NULL; END $do$;\n\
         CREATE SCHEMA IF NOT EXISTS tmpl;\n\
         CREATE TABLE IF NOT EXISTS tmpl.run_config (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           exam_id uuid NOT NULL,\n\
           experiment text NOT NULL,\n\
           mode text NOT NULL,\n\
           variant text NOT NULL CHECK (variant IN ('stream','trace')),\n\
           round int, rep int, plan_seed bigint, mode_order text,\n\
           target_pps double precision, window_s double precision,\n\
           sent bigint NOT NULL, ok bigint,\n\
           started_at timestamptz NOT NULL, stopped_at timestamptz,\n\
           send_elapsed_ns bigint, export_elapsed_ns bigint, note text);\n\
         CREATE TABLE IF NOT EXISTS tmpl.metric (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           config_id uuid NOT NULL, metric text NOT NULL,\n\
           kind text NOT NULL CHECK (kind IN ('counter','span','gauge','histogram')),\n\
           count bigint NOT NULL DEFAULT 0, total_us bigint NOT NULL DEFAULT 0,\n\
           min_us bigint NOT NULL DEFAULT 0, max_us bigint NOT NULL DEFAULT 0,\n\
           current bigint NOT NULL DEFAULT 0, peak bigint NOT NULL DEFAULT 0,\n\
           edges bigint[], buckets bigint[],\n\
           UNIQUE (config_id, metric));\n\
         CREATE TABLE IF NOT EXISTS tmpl.event (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           config_id uuid NOT NULL, seq int NOT NULL,\n\
           kind text NOT NULL, t_us bigint NOT NULL,\n\
           arg0 bigint NOT NULL DEFAULT 0, arg1 bigint NOT NULL DEFAULT 0,\n\
           UNIQUE (config_id, seq));\n\
         CREATE TABLE IF NOT EXISTS tmpl.send (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           config_id uuid NOT NULL, seq bigint,\n\
           t_tx timestamptz NOT NULL, t_rx timestamptz,\n\
           saw_tx boolean, saw_rx boolean, pkt_rx boolean, pkt_n int,\n\
           kernel_tx_ns bigint, kernel_rx_ns bigint, rx_stamp_n int);\n\
         CREATE TABLE IF NOT EXISTS tmpl.slot (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           config_id uuid NOT NULL,\n\
           slot int NOT NULL CHECK (slot BETWEEN 0 AND 255),\n\
           value bigint NOT NULL, UNIQUE (config_id, slot));\n\
         CREATE TABLE IF NOT EXISTS tmpl.ip (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           config_id uuid NOT NULL, key inet NOT NULL, value bigint NOT NULL,\n\
           UNIQUE (config_id, key));\n\
         CREATE TABLE IF NOT EXISTS tmpl.ident (\n\
           id uuid PRIMARY KEY DEFAULT gen_random_uuid(),\n\
           config_id uuid NOT NULL, sent_tokens bytea, recv_tokens bytea,\n\
           UNIQUE (config_id));\n",
    );
    sql
}

/// Extra columns for a table, or none when it is not a config table.
///
/// The exec config carries the workload's input and output, so its extra set
/// lives here rather than in `FAMILIES`: exec is one family with eight
/// workloads, not eight families.
fn extra_for(table: &str) -> Extra {
    for spec in FAMILIES {
        if table == format!("{}_config", spec.family) {
            return spec.config_extra;
        }
    }
    if table == "exec_bench_config" {
        // `result_ok`, not `ok`: the template already has `ok bigint` for the
        // count of passing samples, and `ADD COLUMN IF NOT EXISTS` would skip a
        // second column of the same name, leaving the boolean unset. Two
        // different facts, two names.
        return Extra(&[
            ("workload", "text"),
            ("input", "bytea"),
            ("output", "bytea"),
            ("result_ok", "boolean"),
            ("command_ms", "double precision"),
        ]);
    }
    if table == "case_config" {
        return Extra(&[
            ("image", "text"),
            ("expect", "text"),
            ("map_before", "bigint"),
            ("map_after", "bigint"),
            ("saw_reply", "boolean"),
        ]);
    }
    if table == "case_latency" {
        return Extra(&[
            ("image", "text"),
            ("stopped_ms", "double precision"),
            ("copied_ms", "double precision"),
            ("started_ms", "double precision"),
            ("end_ms", "double precision"),
            ("mcu_us", "bigint"),
            ("disable_rx", "double precision"),
            ("agent_rx", "double precision"),
        ]);
    }
    if table == "hot_swap_config" {
        return Extra(&[
            ("image", "text"),
            ("switched_ms", "double precision"),
            ("end_ms", "double precision"),
            ("mcu_us", "bigint"),
            ("agent_rx", "double precision"),
        ]);
    }
    NO_EXTRA
}

/// Creates one schema's tables over its templates. Idempotent.
///
/// Columns are added one per `ALTER` because a multi-column `ADD COLUMN` has no
/// per-column `IF NOT EXISTS`.
pub fn create_mode_sql(mode: &str) -> String {
    let mut sql = String::new();
    for (name, tmpl, _) in expand(mode) {
        sql.push_str(&format!(
            "CREATE TABLE IF NOT EXISTS {mode}.{name} (LIKE tmpl.{} INCLUDING ALL);\n",
            template_name(tmpl)
        ));
        for (col, ty) in extra_for(&name).0 {
            sql.push_str(&format!(
                "ALTER TABLE {mode}.{name} ADD COLUMN IF NOT EXISTS {col} {ty};\n"
            ));
        }
    }
    sql
}

/// Foreign keys for one schema, run after every table exists.
pub fn keys_mode_sql(mode: &str) -> String {
    let mut sql = String::new();
    for (name, _tmpl, parent) in expand(mode) {
        match parent {
            None => {
                // A config table references the run registry. Dropping first
                // makes a re-run idempotent without a "duplicate constraint"
                // abort in the middle of the pass.
                //
                // Each statement is built with its own format string and joined
                // with an explicit newline: a `\` line continuation swallows the
                // leading space on the next line, which glued the constraint name
                // to the keyword and produced invalid SQL.
                let drop = format!(
                    "ALTER TABLE {mode}.{name} DROP CONSTRAINT IF EXISTS {name}_exam;"
                );
                let add = format!(
                    "ALTER TABLE {mode}.{name} ADD CONSTRAINT {name}_exam \
                     FOREIGN KEY (exam_id) REFERENCES formal.exam(id) ON DELETE CASCADE;"
                );
                sql.push_str(&drop);
                sql.push('\n');
                sql.push_str(&add);
                sql.push('\n');
            }
            Some(p) => {
                let drop = format!(
                    "ALTER TABLE {mode}.{name} DROP CONSTRAINT IF EXISTS {name}_config;"
                );
                let add = format!(
                    "ALTER TABLE {mode}.{name} ADD CONSTRAINT {name}_config \
                     FOREIGN KEY (config_id) REFERENCES {mode}.{p}(id) ON DELETE CASCADE;"
                );
                sql.push_str(&drop);
                sql.push('\n');
                sql.push_str(&add);
                sql.push('\n');
            }
        }
    }
    sql
}

/// Template name for a kind. Shared by the DDL builder and the tests so the two
/// cannot disagree about which template a table copies.
pub const fn template_name(t: Template) -> &'static str {
    t.name()
}

/// Reject a mode name the schema layer does not know.
///
/// Mode names reach SQL as schema identifiers, so this is the one place a bad
/// value is stopped.
pub fn check_mode(mode: &str) -> Result<()> {
    if MODES.contains(&mode) {
        Ok(())
    } else {
        bail!("unknown mode `{mode}`; expected one of {MODES:?}")
    }
}

/// Table names for one mode.
pub fn tables_for(mode: &str) -> Vec<String> {
    expand(mode).into_iter().map(|(n, _, _)| n).collect()
}

/// After creating, check that every expanded table is present.
///
/// A table the expansion names but the DDL missed would take rows at insert time
/// on the device and then fail, which is far more expensive than failing here.
async fn verify_present(client: &Client) -> Result<()> {
    for mode in MODES {
        for name in tables_for(mode) {
            let row = client
                .query_opt(
                    "SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                     WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind = 'r'",
                    &[&mode, &name],
                )
                .await
                .with_context(|| format!("probe {mode}.{name}"))?;
            if row.is_none() {
                anyhow::bail!("{mode}.{name} is in the expansion but was not created");
            }
        }
    }
    Ok(())
}

async fn ensure_schema(client: &Client, name: &str) -> Result<()> {
    if let Err(e) = client
        .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {name};"))
        .await
    {
        let already = e
            .as_db_error()
            .map(|d| d.code() == &tokio_postgres::error::SqlState::DUPLICATE_SCHEMA)
            .unwrap_or(false);
        if !already {
            return Err(e).with_context(|| format!("create schema {name}"));
        }
    }
    Ok(())
}

/// Lock key for the schema bootstrap.
///
/// `CREATE TABLE IF NOT EXISTS` has the same race as `CREATE SCHEMA IF NOT
/// EXISTS`: two sessions can both find the name absent and one then fails on the
/// catalog's unique index. A session-level advisory lock serialises the whole
/// bootstrap, which is a one-time cost per process start.
const BOOTSTRAP_LOCK: i64 = 0x7870_6f73_745f_7363;

/// Creates every schema, template, and table. Idempotent.
///
/// Runs in passes: schemas and templates, then every table with its extra
/// columns, then foreign keys. Keys come last so a table's parent exists no
/// matter what order the expansion produced.
pub async fn ensure(client: &Client) -> Result<()> {
    // Serialise the bootstrap against another process starting at the same time.
    // Released by the same call at the end, including on the error paths below.
    let locked = client
        .query_one("SELECT pg_try_advisory_lock($1)", &[&BOOTSTRAP_LOCK])
        .await
        .context("take bootstrap lock")?
        .get::<_, bool>(0);
    let out = if locked {
        bootstrap(client).await
    } else {
        // Another process holds it, so its bootstrap is already running or done.
        // Waiting is better than failing: the tables this process needs will
        // exist when the lock is released.
        client
            .batch_execute(&format!("SELECT pg_advisory_lock({BOOTSTRAP_LOCK})"))
            .await
            .context("wait for bootstrap lock")?;
        bootstrap(client).await
    };
    let _ = client
        .batch_execute(&format!("SELECT pg_advisory_unlock({BOOTSTRAP_LOCK})"))
        .await;
    out
}

async fn bootstrap(client: &Client) -> Result<()> {
    // `CREATE SCHEMA IF NOT EXISTS` is not race-free either, so each schema is
    // created through a step that tolerates a concurrent creator.
    ensure_schema(client, "tmpl").await?;
    for mode in MODES {
        ensure_schema(client, mode).await?;
    }

    client
        .batch_execute(&template_sql())
        .await
        .context("create templates")?;

    for mode in MODES {
        client
            .batch_execute(&create_mode_sql(mode))
            .await
            .with_context(|| format!("create tables in {mode}"))?;
    }
    for mode in MODES {
        client
            .batch_execute(&keys_mode_sql(mode))
            .await
            .with_context(|| format!("create keys in {mode}"))?;
    }
    client
        .batch_execute(&compare_view_sql())
        .await
        .context("create compare view")?;
    verify_present(client).await
}

/// A view that holds every mode's metrics in one shape.
///
/// A comparison needs three things in every row: which mode produced the number,
/// which experiment, and which metric. The per-mode tables carry the first in
/// their name and the last two in their columns, so a reader has to union five
/// table families by hand to get one comparable series. That hand-written union
/// is where a report loses the mode and becomes unreadable.
///
/// The view is the union. Every row names its mode, so a query cannot drop it.
pub fn compare_view_sql() -> String {
    let mut arms: Vec<String> = Vec::new();
    for mode in MODES {
        for spec in FAMILIES {
            for variant in ["stream", "trace"] {
                arms.push(format!(
                    "SELECT '{mode}'::text AS mode, '{exp}'::text AS experiment, \
                     '{variant}'::text AS variant, \
                     c.workload::text AS workload, \
                     d.metric, d.kind, d.count, d.total_us, d.min_us, d.max_us, \
                     d.current, d.peak, d.edges, d.buckets \
                     FROM {mode}.{exp}_{variant}_data d \
                     JOIN {mode}.{exp}_config c ON c.id = d.config_id",
                    exp = spec.family,
                ));
            }
        }
        for wl in WORKLOADS {
            arms.push(format!(
                "SELECT '{mode}'::text AS mode, 'exec_bench'::text AS experiment, \
                 'trace'::text AS variant, \
                 c.workload::text AS workload, \
                 d.metric, d.kind, d.count, d.total_us, d.min_us, d.max_us, \
                 d.current, d.peak, d.edges, d.buckets \
                 FROM {mode}.exec_bench_{wl}_data d \
                 JOIN {mode}.exec_bench_config c ON c.id = d.config_id"
            ));
        }
    }
    format!(
        "CREATE OR REPLACE VIEW aot.compare AS\n{}\n",
        arms.join("\nUNION ALL\n")
    )
}

/// Every statement `ensure` would run, in order, separated by newlines.
///
/// Test and diagnostic only: `ensure` runs the statements itself, and a host
/// executes this export when it needs to inspect the DDL rather than apply it.
///
/// Exposed so a host can execute the exact DDL the collector uses instead of a
/// copy of it. A second copy would be a second thing to keep in step, and the
/// copy is what a reviewer would end up trusting.
#[cfg(any(test, feature = "host-schema-dump"))]
pub fn all_sql() -> String {
    let mut sql = template_sql();
    for mode in MODES {
        sql.push_str(&format!("CREATE SCHEMA IF NOT EXISTS {mode};\n"));
    }
    for mode in MODES {
        sql.push_str(&create_mode_sql(mode));
    }
    for mode in MODES {
        sql.push_str(&keys_mode_sql(mode));
    }
    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_gets_the_same_family_tables() {
        // A mode missing a family's config table cannot record any run of it,
        // which reads as a silently absent series.
        for mode in MODES {
            let names = tables_for(mode);
            for spec in FAMILIES {
                for suffix in ["config", "stream_data", "trace_data", "trace_event"] {
                    let want = format!("{}_{suffix}", spec.family);
                    assert!(
                        names.contains(&want),
                        "mode {mode} has no {want}; tables are {names:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn case_and_hot_swap_exist_only_where_they_can_run() {
        // The interpreter does not execute RV32IMC machine code, so the switch
        // path has one mode. Creating the tables elsewhere would invite a run
        // that can never fill them.
        assert!(tables_for("aot").iter().any(|n| n == "case_config"));
        for mode in ["rbpf", "hal"] {
            let names = tables_for(mode);
            assert!(!names.iter().any(|n| n == "case_config"), "{mode}");
            assert!(!names.iter().any(|n| n == "hot_swap_config"), "{mode}");
        }
    }

    #[test]
    fn every_data_like_table_names_a_config_parent() {
        for mode in MODES {
            for (name, tmpl, parent) in expand(mode) {
                let data_like = !matches!(tmpl, Template::Config);
                if data_like {
                    assert!(parent.is_some(), "{mode}.{name} has config_id but no parent");
                } else {
                    assert!(parent.is_none(), "{mode}.{name} is a config with a parent");
                }
            }
        }
    }

    #[test]
    fn every_referenced_template_is_created() {
        let sql = template_sql();
        for tmpl in [
            Template::Config,
            Template::Metric,
            Template::Event,
            Template::Send,
            Template::Slot,
            Template::Ip,
            Template::Ident,
        ] {
            let want = format!("tmpl.{}", template_name(tmpl));
            assert!(sql.contains(&want), "{want} never created");
        }
    }

    #[test]
    fn both_variants_have_a_metric_table_per_family() {
        for mode in MODES {
            let names = tables_for(mode);
            for spec in FAMILIES {
                for variant in ["stream", "trace"] {
                    let want = format!("{}_{variant}_data", spec.family);
                    assert!(names.contains(&want), "missing {mode}.{want}");
                }
            }
        }
    }

    #[test]
    fn a_config_table_always_references_the_run_registry() {
        for mode in MODES {
            let sql = keys_mode_sql(mode);
            for spec in FAMILIES {
                let want = format!("{}_config_exam", spec.family);
                assert!(sql.contains(&want), "{mode} has no exam key for {want}");
            }
        }
    }

    /// Write the DDL where a shell can execute it. Ignored by default: it exists
    /// so the real database can be checked against the real generator, and CI
    /// has no database.
    #[test]
    #[ignore]
    fn dump_ddl_to_path_from_env() {
        let path = std::env::var("XPOST_SCHEMA_DUMP").unwrap_or_else(|_| "/tmp/real_schema.sql".into());
        std::fs::write(path, all_sql()).expect("write ddl");
    }

    #[test]
    fn the_generated_ddl_names_every_expanded_table() {
        // This is the check that would have caught the first version of the
        // list, where only one mode had tables: the DDL is what actually runs,
        // so every expanded name must appear in it.
        let sql = all_sql();
        for mode in MODES {
            for name in tables_for(mode) {
                let want = format!("{mode}.{name}");
                assert!(
                    sql.contains(&want),
                    "{want} is expanded but the DDL never creates it"
                );
            }
        }
    }

    #[test]
    fn the_generated_ddl_is_idempotent_by_construction() {
        // Every create is IF NOT EXISTS and every key is dropped before it is
        // added, so running the whole file twice is safe.
        let sql = all_sql();
        let creates = sql.matches("CREATE TABLE").count();
        let guarded = sql.matches("CREATE TABLE IF NOT EXISTS").count();
        assert_eq!(creates, guarded, "a CREATE without IF NOT EXISTS cannot re-run");
        let key_adds = sql.matches("ADD CONSTRAINT").count();
        let key_drops = sql.matches("DROP CONSTRAINT IF EXISTS").count();
        assert_eq!(
            key_adds, key_drops,
            "every added constraint must be dropped first so a re-run does not abort"
        );
    }

    #[test]
    fn expanding_twice_gives_the_same_tables() {
        for mode in MODES {
            assert_eq!(tables_for(mode), tables_for(mode));
        }
    }
}
