//! Optional partitions.csv ↔ XdpTable flash region check (phased).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::metadata::XdpTableMeta;

/// One ESP-IDF-style partition row (name, type, subtype, offset, size, flags).
#[derive(Clone, Debug)]
pub struct PartitionRow {
    pub name: String,
    pub offset: Option<usize>,
    pub size: Option<usize>,
}

/// Parse a minimal `partitions.csv` (skips comments / Name header).
pub fn parse_partitions_csv(path: &Path) -> Result<Vec<PartitionRow>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.to_ascii_lowercase().starts_with("name,") {
            continue;
        }
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() < 5 {
            continue;
        }
        let name = parts[0].to_string();
        let offset = parse_csv_usize(parts[3]);
        let size = parse_csv_usize(parts[4]);
        rows.push(PartitionRow { name, offset, size });
    }
    Ok(rows)
}

fn parse_csv_usize(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return usize::from_str_radix(hex, 16).ok();
    }
    s.parse().ok()
}

/// If `partition` label is set on XdpTable meta, require a matching csv row whose
/// offset/size cover `flash_base..flash_base+flash_len` when both sides have numbers.
pub fn validate_xdp_table_against_partitions(
    table: &XdpTableMeta,
    partitions_csv: &Path,
) -> Result<()> {
    let Some(label) = &table.partition else {
        return Ok(());
    };
    if !partitions_csv.is_file() {
        bail!(
            "xdp_table.partition={label} set but {} missing",
            partitions_csv.display()
        );
    }
    let rows = parse_partitions_csv(partitions_csv)?;
    let Some(row) = rows.iter().find(|r| r.name == *label) else {
        bail!("partitions.csv has no row named `{label}` for xdp_table.partition");
    };
    if let (Some(off), Some(sz)) = (row.offset, row.size) {
        let end = table
            .flash_base
            .checked_add(table.flash_len)
            .ok_or_else(|| anyhow::anyhow!("xdp_table flash range overflow"))?;
        let part_end = off
            .checked_add(sz)
            .ok_or_else(|| anyhow::anyhow!("partition size overflow"))?;
        if table.flash_base < off || end > part_end {
            bail!(
                "xdp_table flash [{:#x}..{:#x}) not inside partition `{label}` [{off:#x}..{part_end:#x})",
                table.flash_base,
                end
            );
        }
    }
    Ok(())
}
