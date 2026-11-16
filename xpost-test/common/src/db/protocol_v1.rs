//! DDL for the new protocol database.
//!
//! Apply [`SQL`] only to an empty database. Table creation is not idempotent.
//! A second apply must fail. This file does not replace the older generator in
//! [`super::schema`].

/// Statements from the xpost-experiments-v1 design, unchanged.
pub const SQL: &str = include_str!("../../../sql/protocol_v1.sql");

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

    #[test]
    fn the_old_generator_is_still_the_three_mode_stream_protocol() {
        assert_eq!(super::super::schema::MODES, ["aot", "rbpf", "hal"]);
        assert!(super::super::schema::all_sql().contains("CREATE TABLE IF NOT EXISTS"));
        assert!(super::super::schema::all_sql().contains("'stream','trace'"));
    }
}
