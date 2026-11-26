//! Read the statement and column inventory from the canonical Protocol V1 DDL.

use anyhow::{Context, Result, bail, ensure};
use std::collections::BTreeMap;

pub(super) const SQL: &str = include_str!("../../../sql/protocol_v1.sql");

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Column {
    pub name: String,
    pub data_type: String,
    pub not_null: bool,
}

pub(super) struct Ddl {
    pub tables: BTreeMap<&'static str, Vec<Column>>,
    pub statements: Vec<Statement>,
}

pub(super) struct Statement {
    pub sql: &'static str,
    pub name: &'static str,
    pub global: bool,
    index: Option<&'static str>,
}

impl Ddl {
    pub fn load() -> Result<Self> {
        let (_, body) = SQL.split_once("\nBEGIN;\n").context("find DDL start")?;
        let body = body
            .trim_end()
            .strip_suffix("\nCOMMIT;")
            .context("find DDL end")?;
        let mut tables = BTreeMap::new();
        let mut statements = Vec::new();
        let mut remainder = body;
        while let Some(end) = statement_end(remainder) {
            let (sql, rest) = remainder.split_at(end);
            remainder = rest;
            let sql = sql
                .lines()
                .skip_while(|line| line.trim().is_empty() || line.starts_with("--"))
                .next()
                .map_or("", |line| &sql[sql.find(line).unwrap()..]);
            let table = sql.starts_with("CREATE TABLE ");
            let global = sql.starts_with("CREATE FUNCTION ") || sql.starts_with("CREATE DOMAIN ");
            let index = sql
                .strip_prefix("CREATE UNIQUE INDEX ")
                .and_then(|s| s.split_whitespace().next());
            let relation = if index.is_some() || sql.starts_with("CREATE TRIGGER ") {
                sql.split_once(" ON release.").map(|(_, name)| name)
            } else {
                sql.split_once(" release.").map(|(_, name)| name)
            };
            ensure!(
                table
                    || global
                    || index.is_some()
                    || sql.starts_with("CREATE TRIGGER ")
                    || sql.starts_with("ALTER TABLE "),
                "unsupported statement in Protocol V1 DDL"
            );
            let name = relation
                .and_then(|s| s.split([' ', '(', '\n', ';']).next())
                .filter(|s| {
                    !s.is_empty()
                        && s.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                })
                .context("read Protocol V1 object name")?;
            if table {
                let columns = columns(sql)
                    .with_context(|| format!("read Protocol V1 columns for release.{name}"))?;
                ensure!(
                    tables.insert(name, columns).is_none(),
                    "duplicate Protocol V1 table {name}"
                );
            }
            statements.push(Statement {
                sql,
                name,
                global,
                index,
            });
        }
        ensure!(remainder.trim().is_empty(), "unterminated Protocol V1 DDL");
        Ok(Self { tables, statements })
    }
}

impl Statement {
    pub fn shadow(&self, prefix: &str) -> String {
        let sql = self.sql.replace("release.", &format!("release.{prefix}"));
        match self.index {
            Some(index) => sql.replacen(index, &format!("{prefix}{index}"), 1),
            None => sql,
        }
    }
}

fn statement_end(sql: &str) -> Option<usize> {
    let mut dollar = false;
    let mut end = 0;
    for line in sql.split_inclusive('\n') {
        end += line.len();
        if line.starts_with("--") {
            continue;
        }
        if line.matches("$$").count() % 2 == 1 {
            dollar = !dollar;
        }
        if !dollar && line.trim_end().ends_with(';') {
            return Some(end);
        }
    }
    None
}

fn columns(sql: &str) -> Result<Vec<Column>> {
    let (_, body) = sql.split_once('(').context("find table body")?;
    let body = body
        .trim_end()
        .strip_suffix(");")
        .context("find table end")?;
    let body = body
        .lines()
        .map(without_comment)
        .collect::<Vec<_>>()
        .join("\n");
    let mut depth = 0_i32;
    let mut quoted = false;
    let mut start = 0;
    let mut items = Vec::new();
    for (idx, ch) in body.char_indices() {
        match ch {
            '\'' => quoted = !quoted,
            '(' | '[' if !quoted => depth += 1,
            ')' | ']' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                items.push(body[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    ensure!(depth == 0 && !quoted, "unbalanced table definition");
    items.push(body[start..].trim());
    let mut primary_key = Vec::new();
    let mut cols = Vec::new();
    for item in items {
        if let Some(key) = item
            .strip_prefix("PRIMARY KEY(")
            .and_then(|s| s.strip_suffix(')'))
        {
            primary_key.extend(key.split(',').map(str::trim));
            continue;
        }
        let (name, rest) = item.split_once([' ', '(']).context("read table item")?;
        if matches!(name, "CHECK" | "FOREIGN" | "UNIQUE") {
            continue;
        }
        if !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            bail!("unsupported column definition {name}");
        }
        let (data_type, clauses) = rest.split_once(' ').unwrap_or((rest, ""));
        let data_type = match data_type {
            "timestamptz" => "timestamp with time zone",
            other => other,
        };
        cols.push(Column {
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            not_null: clauses.starts_with("NOT NULL") || clauses.starts_with("PRIMARY KEY"),
        });
    }
    for column in &mut cols {
        column.not_null |= primary_key.contains(&column.name.as_str());
    }
    Ok(cols)
}

// The canonical DDL uses single-quoted string literals and line comments.
// Adjacent quote bytes toggle twice, which keeps escaped quotes inside a literal.
fn without_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, bytes) in line.as_bytes().windows(2).enumerate() {
        if bytes[0] == b'\'' {
            quoted = !quoted;
        } else if !quoted && bytes == b"--" {
            return &line[..index];
        }
    }
    line
}
