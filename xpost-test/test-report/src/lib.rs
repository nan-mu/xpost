//! Run report types + stdout formatters (table default, JSON via `--json`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub ok: bool,
    pub iface: String,
    pub esp_ip: String,
    pub tests: Vec<TestReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestReport {
    pub name: String,
    pub ok: bool,
    pub cases: Vec<CaseReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaseReport {
    pub test: String,
    pub case: String,
    pub expect_echo: bool,
    pub got_echo: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_bounce: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub got_bounce: bool,
    pub pass: bool,
    pub t0_ns: Option<u64>,
    pub t1_ns: Option<u64>,
    pub delta_ns: Option<u64>,
    pub seq: u64,
    pub src_ip: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dst_ip: String,
    /// TX-mark stages: `Some(0)` = expect passthrough (non-magic sport); `Some(0x4254/55)` = mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_sport: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub got_sport: Option<u16>,
    /// Plan C letters: P / H / D (overrides sport/echo tags when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_cell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub got_cell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `T0/a` → (`T0`, `a`) when role is a single lowercase letter.
fn parse_stage_role(case: &str) -> Option<(&str, char)> {
    let (stage, role) = case.split_once('/')?;
    let mut chars = role.chars();
    let c = chars.next()?;
    if chars.next().is_none() && c.is_ascii_lowercase() {
        Some((stage, c))
    } else {
        None
    }
}

impl RunReport {
    pub fn from_tests(iface: String, esp_ip: String, tests: Vec<TestReport>) -> Self {
        let ok = tests.iter().all(|t| t.ok);
        Self {
            ok,
            iface,
            esp_ip,
            tests,
        }
    }

    /// Human-readable table: peers legend + stage×role when cases look like `T0/a`;
    /// otherwise one case per line (legacy).
    pub fn format_table(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "test  iface={}  esp={}  {}",
            self.iface,
            self.esp_ip,
            if self.ok { "PASS" } else { "FAIL" }
        );

        if let Some(legend) = peers_legend(&self.tests) {
            let _ = writeln!(out, "{legend}");
        }

        for t in &self.tests {
            let _ = writeln!(out);
            let _ = writeln!(out, "[{}] {}", if t.ok { "PASS" } else { "FAIL" }, t.name);
            if t.name == "test0" {
                continue;
            }
            if stage_role_cases(&t.cases).next().is_some() {
                format_stage_table(&mut out, &t.cases);
            } else {
                format_legacy_table(&mut out, &t.cases);
            }
        }

        let n_ok = self.tests.iter().filter(|t| t.ok).count();
        let n_case_ok = self
            .tests
            .iter()
            .flat_map(|t| t.cases.iter())
            .filter(|c| c.pass)
            .count();
        let n_case = self.tests.iter().map(|t| t.cases.len()).sum::<usize>();
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "summary  tests={}/{}  cases={}/{}  {}",
            n_ok,
            self.tests.len(),
            n_case_ok,
            n_case,
            if self.ok { "PASS" } else { "FAIL" }
        );
        out
    }
}

fn stage_role_cases(cases: &[CaseReport]) -> impl Iterator<Item = (&CaseReport, &str, char)> {
    cases.iter().filter_map(|c| {
        let (stage, role) = parse_stage_role(&c.case)?;
        Some((c, stage, role))
    })
}

fn peers_legend(tests: &[TestReport]) -> Option<String> {
    let mut first_ip: BTreeMap<char, String> = BTreeMap::new();
    for t in tests {
        for (c, _, role) in stage_role_cases(&t.cases) {
            first_ip.entry(role).or_insert_with(|| c.src_ip.clone());
        }
    }
    if first_ip.is_empty() {
        return None;
    }
    let mut s = String::from("peers ");
    for (i, (role, ip)) in first_ip.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{role}={ip}");
    }
    Some(s)
}

fn format_stage_table(out: &mut String, cases: &[CaseReport]) {
    // role → (first-seen stage order preserved; cells by role)
    let mut roles: Vec<char> = Vec::new();
    let mut stages: Vec<String> = Vec::new();
    // (stage_idx, role) → case
    let mut grid: BTreeMap<(usize, char), &CaseReport> = BTreeMap::new();

    for (c, stage, role) in stage_role_cases(cases) {
        if !roles.contains(&role) {
            roles.push(role);
        }
        let stage_idx = match stages.iter().position(|s| s == stage) {
            Some(i) => i,
            None => {
                stages.push(stage.to_string());
                stages.len() - 1
            }
        };
        grid.entry((stage_idx, role)).or_insert(c);
    }
    roles.sort_unstable(); // a, b, c, …

    // Header: STAGE  a    b    c      Δa     Δb     Δc
    let mut header = String::from("STAGE");
    for r in &roles {
        let _ = write!(header, "  {:<5}", r);
    }
    for r in &roles {
        let _ = write!(header, "  {:>6}", format!("Δ{r}"));
    }
    let _ = writeln!(out, "{header}");

    for (si, stage) in stages.iter().enumerate() {
        let mut line = format!("{:<5}", trunc(stage, 5));
        for r in &roles {
            let cell = match grid.get(&(si, *r)) {
                Some(c) => {
                    let tag = cell_tag(c);
                    if c.pass { tag } else { format!("{tag}!") }
                }
                None => "-".into(),
            };
            let _ = write!(line, "  {:<5}", cell);
        }
        for r in &roles {
            let delta = grid
                .get(&(si, *r))
                .map(|c| format_delta(c.delta_ns))
                .unwrap_or_else(|| "-".into());
            let _ = write!(line, "  {:>6}", trunc(&delta, 6));
        }
        let _ = writeln!(out, "{line}");
    }

    // Non stage/role cases (e.g. T1/create_jmp) — compact legacy lines.
    let orphans: Vec<CaseReport> = cases
        .iter()
        .filter(|c| parse_stage_role(&c.case).is_none())
        .cloned()
        .collect();
    if !orphans.is_empty() {
        let _ = writeln!(out, "OTHER");
        format_legacy_table(out, &orphans);
    }
}

fn format_legacy_table(out: &mut String, cases: &[CaseReport]) {
    let _ = writeln!(
        out,
        "{:<10} {:<4} {:<3} {:<3} {:>10}  {:<15} {:<15} ERR",
        "CASE", "RES", "EXP", "GOT", "Δ", "SRC", "DST"
    );
    for c in cases {
        let delta = format_delta(c.delta_ns);
        let err = c.error.as_deref().unwrap_or("");
        let _ = writeln!(
            out,
            "{:<10} {:<4} {:<3} {:<3} {:>10}  {:<15} {:<15} {}",
            trunc(&c.case, 10),
            if c.pass { "ok" } else { "FAIL" },
            outcome_tag(c.expect_echo, c.expect_bounce),
            outcome_tag(c.got_echo, c.got_bounce),
            delta,
            trunc(&c.src_ip, 15),
            trunc(&c.dst_ip, 15),
            err
        );
    }
}

fn cell_tag(c: &CaseReport) -> String {
    if let (Some(exp), Some(got)) = (c.expect_cell.as_deref(), c.got_cell.as_deref()) {
        format!("{exp}/{got}")
    } else if c.expect_sport.is_some() {
        format!(
            "{}/{}",
            mark_expect_tag(c.expect_sport),
            mark_got_tag(c.got_echo, c.got_sport)
        )
    } else {
        format!(
            "{}/{}",
            outcome_tag(c.expect_echo, c.expect_bounce),
            outcome_tag(c.got_echo, c.got_bounce)
        )
    }
}

fn mark_expect_tag(expect: Option<u16>) -> &'static str {
    match expect {
        Some(0x4254) => "M",
        Some(0x4255) => "N",
        Some(0) => "P",
        _ => "?",
    }
}

fn mark_got_tag(got_echo: bool, sport: Option<u16>) -> &'static str {
    if !got_echo {
        return "X";
    }
    match sport {
        Some(0x4254) => "M",
        Some(0x4255) => "N",
        _ => "P",
    }
}

fn outcome_tag(echo: bool, bounce: bool) -> &'static str {
    match (echo, bounce) {
        (true, false) => "E",
        (false, true) => "B",
        (false, false) => "D",
        (true, true) => "EB",
    }
}

fn format_delta(ns: Option<u64>) -> String {
    match ns {
        None => "-".into(),
        Some(n) if n >= 1_000_000 => format!("{:.1}ms", n as f64 / 1_000_000.0),
        Some(n) if n >= 1_000 => format!("{:.0}µs", n as f64 / 1_000.0),
        Some(n) => format!("{n}ns"),
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_abc_report() -> RunReport {
        let mk =
            |case: &str, src: &str, exp_e: bool, got_e: bool, exp_b: bool, got_b: bool, d: u64| {
                CaseReport {
                    test: "tail_call_abc".into(),
                    case: case.into(),
                    expect_echo: exp_e,
                    got_echo: got_e,
                    expect_bounce: exp_b,
                    got_bounce: got_b,
                    pass: exp_e == got_e && exp_b == got_b && !(got_e && got_b),
                    t0_ns: Some(0),
                    t1_ns: Some(d),
                    delta_ns: Some(d),
                    seq: 1,
                    src_ip: src.into(),
                    dst_ip: "192.168.110.103".into(),
                    expect_sport: None,
                    got_sport: None,
                    expect_cell: None,
                    got_cell: None,
                    error: None,
                }
            };
        // Deliberately push c before a for T0 to prove column order is a,b,c not push order.
        RunReport {
            ok: true,
            iface: "br-lan".into(),
            esp_ip: "192.168.110.103".into(),
            tests: vec![TestReport {
                name: "tail_call_abc".into(),
                ok: true,
                cases: vec![
                    mk(
                        "T0/c",
                        "192.168.110.247",
                        true,
                        true,
                        false,
                        false,
                        4_911_657,
                    ),
                    mk(
                        "T0/a",
                        "192.168.110.19",
                        true,
                        true,
                        false,
                        false,
                        12_803_500,
                    ),
                    mk(
                        "T0/b",
                        "192.168.110.50",
                        true,
                        true,
                        false,
                        false,
                        5_299_597,
                    ),
                    mk(
                        "T2/a",
                        "192.168.110.19",
                        true,
                        true,
                        false,
                        false,
                        4_000_000,
                    ),
                    mk(
                        "T2/b",
                        "192.168.110.50",
                        false,
                        false,
                        true,
                        true,
                        3_000_000,
                    ),
                    mk(
                        "T2/c",
                        "192.168.110.247",
                        true,
                        true,
                        false,
                        false,
                        4_100_000,
                    ),
                ],
            }],
        }
    }

    #[test]
    fn stage_table_columns_are_abc_not_ip_order() {
        let table = sample_abc_report().format_table();
        assert!(
            table.contains("peers a=192.168.110.19 b=192.168.110.50 c=192.168.110.247"),
            "legend must list a then b then c:\n{table}"
        );
        let header = table
            .lines()
            .find(|l| l.starts_with("STAGE"))
            .expect("STAGE header");
        let a_pos = header.find("  a ").or_else(|| header.find("a  "));
        let b_pos = header.find("  b ").or_else(|| header.find("b  "));
        let c_pos = header.find("  c ").or_else(|| header.find("c  "));
        assert!(
            a_pos.is_some() && b_pos.is_some() && c_pos.is_some(),
            "header={header}"
        );
        assert!(a_pos < b_pos && b_pos < c_pos, "roles not a→b→c: {header}");
        assert!(
            !table.lines().any(|l| l.starts_with("CASE")),
            "must not use legacy CASE table for stage/role cases:\n{table}"
        );
        let t0 = table.lines().find(|l| l.starts_with("T0")).expect("T0 row");
        // After STAGE label: three E/E cells in a,b,c order
        assert!(t0.contains("E/E"), "t0={t0}");
    }

    #[test]
    fn cycle0_json_formats_stage_abc() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/evidence/cycle0_tail_call_abc.json"
        );
        let raw = std::fs::read_to_string(path).expect("read cycle0 json");
        let report: RunReport = serde_json::from_str(&raw).expect("parse cycle0 json");
        let table = report.format_table();
        assert!(table.contains("peers a="), "missing peers legend:\n{table}");
        assert!(table.contains(" b="), "missing b in legend:\n{table}");
        assert!(table.contains(" c="), "missing c in legend:\n{table}");
        let peers = table.lines().find(|l| l.starts_with("peers")).unwrap();
        let ia = peers.find("a=").unwrap();
        let ib = peers.find(" b=").unwrap();
        let ic = peers.find(" c=").unwrap();
        assert!(ia < ib && ib < ic, "peers order: {peers}");
        let header = table.lines().find(|l| l.starts_with("STAGE")).unwrap();
        let a = header.find('a').unwrap();
        let b = header.find('b').unwrap();
        let c = header.find('c').unwrap();
        assert!(a < b && b < c, "header={header}");
        for stage in ["T0", "T1", "T2", "T3", "T4", "T5"] {
            assert!(
                table.lines().any(|l| l.starts_with(stage)),
                "missing stage {stage}:\n{table}"
            );
        }
    }

    #[test]
    fn legacy_table_when_no_stage_role() {
        let report = RunReport {
            ok: true,
            iface: "br-lan".into(),
            esp_ip: "192.168.110.103".into(),
            tests: vec![TestReport {
                name: "firewall_echo".into(),
                ok: true,
                cases: vec![CaseReport {
                    test: "firewall_echo".into(),
                    case: "allowed".into(),
                    expect_echo: true,
                    got_echo: true,
                    expect_bounce: false,
                    got_bounce: false,
                    pass: true,
                    t0_ns: Some(0),
                    t1_ns: Some(1_000_000),
                    delta_ns: Some(1_000_000),
                    seq: 1,
                    src_ip: "192.168.110.96".into(),
                    dst_ip: "192.168.110.103".into(),
                    expect_sport: None,
                    got_sport: None,
                    expect_cell: None,
                    got_cell: None,
                    error: None,
                }],
            }],
        };
        let table = report.format_table();
        assert!(
            !table.contains("peers "),
            "no peers without roles:\n{table}"
        );
        assert!(table.contains("CASE"), "legacy header:\n{table}");
        assert!(table.contains("allowed"), "legacy case:\n{table}");
    }

    #[test]
    fn test0_skips_legacy_case_table() {
        let report = RunReport {
            ok: false,
            iface: "eth0".into(),
            esp_ip: "192.168.110.103".into(),
            tests: vec![TestReport {
                name: "test0".into(),
                ok: false,
                cases: vec![CaseReport {
                    test: "test0".into(),
                    case: "delete-rx".into(),
                    pass: false,
                    ..Default::default()
                }],
            }],
        };
        let table = report.format_table();
        assert!(
            !table.lines().any(|l| l.starts_with("CASE")),
            "test0 must not dump CASE table:\n{table}"
        );
        assert!(table.contains("[FAIL] test0"), "{table}");
    }
}
