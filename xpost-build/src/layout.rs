//! Compile IRAM layout from board `layout.toml` `[arena]`/`[helper]`/`[slots]` → `mem.toml`.
//!
//! Intent lives in `layout.toml`. [`doctor`](doctor) validates and writes the generated
//! `mem.toml`. [`crate::mem_map`] only consumes that file.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::parse_addr_value;

/// Inclusive slot count bounds for ESP32-C3 patch arena.
pub const MIN_SLOT_COUNT: usize = 2;
pub const MAX_SLOT_COUNT: usize = 4;

/// Known mount roles (`mount` string in config / mem.toml).
pub const MOUNT_CALL: &str = "call";
pub const MOUNT_RX: &str = "rx";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountKind {
    Call,
    Rx,
}

impl MountKind {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            MOUNT_CALL => Ok(Self::Call),
            MOUNT_RX => Ok(Self::Rx),
            other => bail!("unknown mount `{other}` (known: call, rx)"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Call => MOUNT_CALL,
            Self::Rx => MOUNT_RX,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub base: usize,
    pub length: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotRegion {
    pub name: String,
    pub base: usize,
    pub length: usize,
    pub mount: MountKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub iram_base: usize,
    pub arena_end: usize,
    pub helper: Region,
    pub hot_regions: Vec<HotRegion>,
}

impl Layout {
    pub fn arena_len(&self) -> usize {
        self.arena_end.saturating_sub(self.iram_base)
    }

    pub fn used_len(&self) -> usize {
        self.helper.length + self.hot_regions.iter().map(|r| r.length).sum::<usize>()
    }

    pub fn remaining(&self) -> usize {
        self.arena_len().saturating_sub(self.used_len())
    }
}

pub struct LayoutReport {
    pub layout: Layout,
    pub mem_path: PathBuf,
    pub lines: Vec<String>,
}

/// Validate board `layout.toml` and write `mem.toml` (doctor / layout CLI).
pub fn doctor(board_dir: &Path) -> Result<LayoutReport> {
    let layout = compute_from_file(&board_dir.join("layout.toml"))?;
    cross_check_firmware_config(board_dir, board_dir)?;
    let mem_path = board_dir.join("mem.toml");
    write_mem_toml(&mem_path, &layout)?;
    let lines = report_lines(&layout);
    Ok(LayoutReport {
        layout,
        mem_path,
        lines,
    })
}

/// Compute layout from a TOML file (`layout.toml` or leftover `config.toml` sections).
pub fn compute_from_file(path: &Path) -> Result<Layout> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let root: toml::Table = text
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    compute_from_root(&root)
}

fn parse_usize_field(table: &toml::Table, key: &str, section: &str) -> Result<usize> {
    let v = table
        .get(key)
        .ok_or_else(|| anyhow::anyhow!("layout: [{section}] missing {key}"))?;
    Ok(parse_addr_value(v, key, section))
}

pub fn compute_from_root(root: &toml::Table) -> Result<Layout> {
    let arena = root
        .get("arena")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("layout: missing [arena]"))?;
    let helper_t = root
        .get("helper")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("layout: missing [helper]"))?;
    let slots_t = root
        .get("slots")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("layout: missing [slots]"))?;

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

    // Named slots: sort keys for stable IRAM order.
    let mut names: Vec<&str> = slots_t
        .keys()
        .map(|k| k.as_str())
        .filter(|k| !k.is_empty())
        .collect();
    names.sort_unstable();

    let count = names.len();
    if !(MIN_SLOT_COUNT..=MAX_SLOT_COUNT).contains(&count) {
        bail!("[slots] count must be in {MIN_SLOT_COUNT}..={MAX_SLOT_COUNT}, got {count}");
    }

    let arena_len = arena_end - iram_base;
    if helper_len >= arena_len {
        bail!("helper.length {helper_len:#x} leaves no room for hot slots in arena {arena_len:#x}");
    }
    let hot_budget = arena_len - helper_len;

    let mut lengths_opt: Vec<Option<usize>> = Vec::with_capacity(count);
    let mut mounts: Vec<MountKind> = Vec::with_capacity(count);
    let mut any_len = false;
    let mut all_len = true;

    for name in &names {
        let entry = slots_t
            .get(*name)
            .and_then(|v| v.as_table())
            .ok_or_else(|| anyhow::anyhow!("layout: [slots].{name} must be an inline table"))?;
        for (key, _) in entry {
            if !matches!(key.as_str(), "length" | "mount") {
                bail!("layout: [slots].{name} unknown key `{key}`");
            }
        }
        let length = match entry.get("length") {
            Some(v) => {
                any_len = true;
                let n = parse_addr_value(v, "length", &format!("slots.{name}"));
                if n == 0 || !n.is_multiple_of(4) {
                    bail!("[slots].{name}.length must be > 0 and 4-byte aligned");
                }
                Some(n)
            }
            None => {
                all_len = false;
                None
            }
        };
        lengths_opt.push(length);

        let mount = match entry.get("mount") {
            Some(v) => {
                let s = v.as_str().ok_or_else(|| {
                    anyhow::anyhow!("layout: [slots].{name}.mount must be a string")
                })?;
                MountKind::parse(s)?
            }
            None => MountKind::Call,
        };
        mounts.push(mount);
    }

    if any_len && !all_len {
        bail!("layout: [slots] either set length on every slot or omit all for equal-split");
    }

    let lengths: Vec<usize> = if all_len {
        lengths_opt.into_iter().map(|o| o.unwrap()).collect()
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

    validate_mounts(&mounts)?;

    let helper = Region {
        base: iram_base,
        length: helper_len,
    };
    let mut cursor = iram_base + helper_len;
    let mut hot_regions = Vec::with_capacity(count);
    for (i, name) in names.iter().enumerate() {
        hot_regions.push(HotRegion {
            name: (*name).to_string(),
            base: cursor,
            length: lengths[i],
            mount: mounts[i],
        });
        cursor += lengths[i];
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
    validate_abut(&layout)?;
    Ok(layout)
}

fn validate_mounts(mounts: &[MountKind]) -> Result<()> {
    let mut rx = 0usize;
    for m in mounts {
        match m {
            MountKind::Call => {}
            MountKind::Rx => rx += 1,
        }
    }
    if rx > 1 {
        bail!("mount `rx` may appear on at most one slot, found {rx}");
    }
    Ok(())
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

fn validate_abut(layout: &Layout) -> Result<()> {
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
                "hot_regions[{i}] (`{}`) base {base:#x} must abut previous end {expect:#x}",
                r.name,
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
        "# @generated by xtask doctor from layout.toml — do not edit by hand.\n\
         # AOT linker scripts / xpost board codegen consume this file.\n\
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
        writeln!(out, "name = \"{}\"", r.name).unwrap();
        writeln!(out, "base = {:#x}", r.base).unwrap();
        writeln!(out, "length = {:#x}", r.length).unwrap();
        writeln!(out, "mount = \"{}\"", r.mount.as_str()).unwrap();
        if i + 1 < layout.hot_regions.len() {
            writeln!(out).unwrap();
        }
    }

    fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Parse an existing `mem.toml` into [`Layout`] (for consistency checks).
pub fn parse_mem_toml(path: &Path) -> Result<Layout> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let root: toml::Table = text.parse().with_context(|| "failed to parse mem.toml")?;

    let arena = root
        .get("arena")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("mem.toml: missing [arena]"))?;
    let helper_t = root
        .get("helper")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("mem.toml: missing [helper]"))?;

    let iram_base = parse_addr_value(
        arena
            .get("iram_base")
            .ok_or_else(|| anyhow::anyhow!("mem.toml: [arena] missing iram_base"))?,
        "iram_base",
        "arena",
    );
    let arena_end = parse_addr_value(
        arena
            .get("arena_end")
            .ok_or_else(|| anyhow::anyhow!("mem.toml: [arena] missing arena_end"))?,
        "arena_end",
        "arena",
    );
    let helper = Region {
        base: parse_addr_value(
            helper_t
                .get("base")
                .ok_or_else(|| anyhow::anyhow!("mem.toml: [helper] missing base"))?,
            "base",
            "helper",
        ),
        length: parse_addr_value(
            helper_t
                .get("length")
                .ok_or_else(|| anyhow::anyhow!("mem.toml: [helper] missing length"))?,
            "length",
            "helper",
        ),
    };

    let arr = root
        .get("hot_regions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("mem.toml: missing [[hot_regions]]"))?;
    let mut hot_regions = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let table = entry
            .as_table()
            .ok_or_else(|| anyhow::anyhow!("mem.toml: hot_regions[{i}] must be a table"))?;
        let name = table
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("mem.toml: hot_regions[{i}].name required"))?
            .to_string();
        let base = parse_addr_value(
            table
                .get("base")
                .ok_or_else(|| anyhow::anyhow!("mem.toml: hot_regions[{i}] missing base"))?,
            "base",
            "hot_regions",
        );
        let length = parse_addr_value(
            table
                .get("length")
                .ok_or_else(|| anyhow::anyhow!("mem.toml: hot_regions[{i}] missing length"))?,
            "length",
            "hot_regions",
        );
        let mount_s = table
            .get("mount")
            .and_then(|v| v.as_str())
            .unwrap_or(MOUNT_CALL);
        let mount = MountKind::parse(mount_s)?;
        hot_regions.push(HotRegion {
            name,
            base,
            length,
            mount,
        });
    }

    let layout = Layout {
        iram_base,
        arena_end,
        helper,
        hot_regions,
    };
    validate_abut(&layout)?;
    validate_mounts(
        &layout
            .hot_regions
            .iter()
            .map(|r| r.mount)
            .collect::<Vec<_>>(),
    )?;
    Ok(layout)
}

/// Ensure on-disk `mem.toml` matches `layout.toml` in the runtime crate dir.
pub fn assert_mem_matches_layout(runtime_dir: &Path) -> Result<()> {
    let expected = compute_from_file(&runtime_dir.join("layout.toml"))?;
    let mem_path = runtime_dir.join("mem.toml");
    if !mem_path.exists() {
        bail!("mem.toml missing — run `cargo xtask doctor` to generate it from layout.toml");
    }
    let actual = parse_mem_toml(&mem_path)?;
    if actual != expected {
        bail!(
            "mem.toml is out of sync with layout.toml [arena]/[helper]/[slots]\n\
             run: cargo xtask doctor"
        );
    }
    Ok(())
}

/// If board `config.toml` still has `[arena]`/`[helper]`/`[slots]`, they must
/// match `layout.toml`.
pub fn cross_check_firmware_config(board_dir: &Path, layout_dir: &Path) -> Result<()> {
    let path = board_dir.join("config.toml");
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let root: toml::Table = text
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let has_layout =
        root.get("arena").is_some() || root.get("helper").is_some() || root.get("slots").is_some();
    if !has_layout {
        return Ok(());
    }
    let from_cfg = compute_from_root(&root)?;
    let from_layout = compute_from_file(&layout_dir.join("layout.toml"))?;
    if from_cfg != from_layout {
        bail!(
            "config.toml [arena]/[helper]/[slots] disagrees with layout.toml\n\
             layout lives beside the example; remove the config.toml copy or make them match"
        );
    }
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
        lines.push(format!(
            "slot[{i}] name={} mount={} base={:#x} len={:#x}",
            r.name,
            r.mount.as_str(),
            r.base,
            r.length
        ));
    }
    lines.push(format!(
        "used={:#x} remaining={:#x}",
        layout.used_len(),
        layout.remaining()
    ));
    lines
}
