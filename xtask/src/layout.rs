//! Compile patch IRAM layout from `patch_layout.toml` → `mem.toml`.
//!
//! This module owns the layout *decision* (slot count, split, validation).
//! [`crate::mem_map`] only consumes the generated `mem.toml`.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::util::parse_addr_value;

/// Inclusive slot count bounds for ESP32-C3 patch arena.
pub const MIN_SLOT_COUNT: usize = 2;
pub const MAX_SLOT_COUNT: usize = 4;

#[derive(Clone, Debug)]
pub struct Region {
    pub base: usize,
    pub length: usize,
}

#[derive(Clone, Debug)]
pub struct Layout {
    pub iram_base: usize,
    pub arena_end: usize,
    pub helper: Region,
    pub hot_regions: Vec<Region>,
}

impl Layout {
    pub fn arena_len(&self) -> usize {
        self.arena_end.saturating_sub(self.iram_base)
    }

    pub fn used_len(&self) -> usize {
        self.helper.length + self.hot_regions.iter().map(|r| r.length).sum::<usize>()
    }

    /// Bytes of arena not covered by helper+slots (should be 0 when filling arena).
    pub fn remaining(&self) -> usize {
        self.arena_len().saturating_sub(self.used_len())
    }
}

pub struct LayoutReport {
    pub layout: Layout,
    pub mem_path: std::path::PathBuf,
    pub lines: Vec<String>,
}

/// Read intent, compute layout, write `mem.toml`, return printable report.
pub fn apply(manifest_dir: &Path) -> Result<LayoutReport> {
    let intent_path = manifest_dir.join("patch_layout.toml");
    let layout = compute_from_path(&intent_path)?;
    let mem_path = manifest_dir.join("mem.toml");
    write_mem_toml(&mem_path, &layout)?;
    let lines = report_lines(&layout);
    Ok(LayoutReport {
        layout,
        mem_path,
        lines,
    })
}

pub fn compute_from_path(intent_path: &Path) -> Result<Layout> {
    let text = fs::read_to_string(intent_path)
        .with_context(|| format!("failed to read {}", intent_path.display()))?;
    let root: toml::Table = text
        .parse()
        .with_context(|| format!("failed to parse {}", intent_path.display()))?;
    compute(&root)
}

fn parse_usize_field(table: &toml::Table, key: &str, section: &str) -> Result<usize> {
    let v = table
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("patch_layout.toml: [{section}] missing {key}"))?;
    Ok(parse_addr_value(v, key, section))
}

pub fn compute(root: &toml::Table) -> Result<Layout> {
    let arena = root
        .get("arena")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("patch_layout.toml: missing [arena]"))?;
    let helper_t = root
        .get("helper")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("patch_layout.toml: missing [helper]"))?;
    let slots_t = root
        .get("slots")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("patch_layout.toml: missing [slots]"))?;

    let iram_base = parse_usize_field(arena, "iram_base", "arena")?;
    let arena_end = parse_usize_field(arena, "arena_end", "arena")?;
    let helper_len = parse_usize_field(helper_t, "length", "helper")?;

    if arena_end <= iram_base {
        bail!("arena_end ({arena_end:#x}) must be > iram_base ({iram_base:#x})");
    }
    if !iram_base.is_multiple_of(4) || !arena_end.is_multiple_of(4) {
        bail!("iram_base and arena_end must be 4-byte aligned");
    }
    if helper_len == 0 || !helper_len.is_multiple_of(4) {
        bail!("helper.length must be > 0 and 4-byte aligned");
    }

    let count = slots_t
        .get("count")
        .and_then(|v| v.as_integer())
        .ok_or_else(|| anyhow::anyhow!("patch_layout.toml: [slots].count required"))?
        as usize;
    if !(MIN_SLOT_COUNT..=MAX_SLOT_COUNT).contains(&count) {
        bail!("[slots].count must be in {MIN_SLOT_COUNT}..={MAX_SLOT_COUNT}, got {count}");
    }

    let arena_len = arena_end - iram_base;
    if helper_len >= arena_len {
        bail!("helper.length {helper_len:#x} leaves no room for hot slots in arena {arena_len:#x}");
    }
    let hot_budget = arena_len - helper_len;

    let lengths: Vec<usize> = if let Some(arr) = slots_t.get("lengths").and_then(|v| v.as_array()) {
        if arr.len() != count {
            bail!("[slots].lengths length {} != count {count}", arr.len());
        }
        arr.iter()
            .enumerate()
            .map(|(i, v)| {
                let n = parse_addr_value(v, "lengths", "slots");
                if n == 0 || !n.is_multiple_of(4) {
                    bail!("[slots].lengths[{i}] must be > 0 and 4-byte aligned");
                }
                Ok(n)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        equal_split(hot_budget, count)?
    };

    let sum: usize = lengths.iter().sum();
    if sum != hot_budget {
        bail!(
            "hot slot lengths sum {sum:#x} != arena remainder {hot_budget:#x} \
             (helper={helper_len:#x}, arena={arena_len:#x})"
        );
    }

    let helper = Region {
        base: iram_base,
        length: helper_len,
    };
    let mut cursor = iram_base + helper_len;
    let mut hot_regions = Vec::with_capacity(count);
    for &length in &lengths {
        hot_regions.push(Region {
            base: cursor,
            length,
        });
        cursor += length;
    }
    if cursor != arena_end {
        bail!("internal layout error: cursor {cursor:#x} != arena_end {arena_end:#x}");
    }

    let layout = Layout {
        iram_base,
        arena_end,
        helper,
        hot_regions,
    };
    validate(&layout)?;
    Ok(layout)
}

fn equal_split(budget: usize, count: usize) -> Result<Vec<usize>> {
    if !budget.is_multiple_of(4) {
        bail!("hot budget {budget:#x} is not 4-byte aligned");
    }
    let unit = budget / 4;
    if unit < count {
        bail!("arena remainder {budget:#x} too small to split into {count} 4-byte-aligned slots");
    }
    let base_units = unit / count;
    let rem_units = unit % count;
    // Give remainder 4-byte chunks to later slots (matches historical A smaller / B larger
    // when count=2 and budget=0x1E00: 0xE00 + 0x1000).
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let extra = if i >= count - rem_units { 1 } else { 0 };
        let len = (base_units + extra) * 4;
        if len == 0 {
            bail!("equal split produced zero-length slot {i}");
        }
        out.push(len);
    }
    Ok(out)
}

fn validate(layout: &Layout) -> Result<()> {
    if !layout.helper.base.is_multiple_of(4) || layout.helper.length == 0 {
        bail!("helper base/length invalid");
    }
    let mut expect = layout.helper.base + layout.helper.length;
    for (i, r) in layout.hot_regions.iter().enumerate() {
        if !r.base.is_multiple_of(4) || r.length == 0 || !r.length.is_multiple_of(4) {
            bail!("hot_regions[{i}] base/length must be >0 and 4-byte aligned");
        }
        if r.base != expect {
            bail!(
                "hot_regions[{i}] base {base:#x} must abut previous end {expect:#x}",
                base = r.base
            );
        }
        expect = r.base + r.length;
    }
    if expect != layout.arena_end {
        bail!(
            "layout end {expect:#x} != arena_end {:#x}",
            layout.arena_end
        );
    }
    Ok(())
}

pub fn write_mem_toml(path: &Path, layout: &Layout) -> Result<()> {
    let mut out = String::new();
    writeln!(
        out,
        "# @generated by xtask layout from patch_layout.toml — do not edit by hand.\n\
         # AOT linker scripts are generated by libbpf-mcu from this file.\n\
         # Arena [{base:#x}, {end:#x}); do not extend past arena_end (.trap).\n",
        base = layout.iram_base,
        end = layout.arena_end
    )
    .unwrap();

    writeln!(out, "[arena]").unwrap();
    writeln!(out, "iram_base = {:#x}", layout.iram_base).unwrap();
    writeln!(out, "arena_end = {:#x}", layout.arena_end).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "[helper]").unwrap();
    writeln!(out, "base = {:#x}", layout.helper.base).unwrap();
    writeln!(out, "length = {:#x}", layout.helper.length).unwrap();
    writeln!(out).unwrap();

    for (i, r) in layout.hot_regions.iter().enumerate() {
        writeln!(out, "[[hot_regions]]").unwrap();
        writeln!(out, "index = {i}").unwrap();
        writeln!(out, "base = {:#x}", r.base).unwrap();
        writeln!(out, "length = {:#x}", r.length).unwrap();
        if i + 1 < layout.hot_regions.len() {
            writeln!(out).unwrap();
        }
    }

    fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn report_lines(layout: &Layout) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!(
        "arena [{:#x}, {:#x}) len={:#x}",
        layout.iram_base,
        layout.arena_end,
        layout.arena_len()
    ));
    lines.push(format!(
        "helper base={:#x} len={:#x}",
        layout.helper.base, layout.helper.length
    ));
    for (i, r) in layout.hot_regions.iter().enumerate() {
        lines.push(format!("slot[{i}] base={:#x} len={:#x}", r.base, r.length));
    }
    lines.push(format!(
        "used={:#x} remaining={:#x}",
        layout.used_len(),
        layout.remaining()
    ));
    lines
}
