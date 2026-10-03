//! Compile IRAM layout from board intent `layout.toml` (`[arena]`/`[helper]`/`[slots]`).
//!
//! Intent lives in board `layout.toml`. [`doctor`](doctor) validates and may export the
//! libbpf-mcu contract schema (artifact `layout.toml`, not overwriting intent).
//! [`crate::mem_map`] consumes the in-memory [`Layout`].

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::util::parse_addr_value;

/// Inclusive slot count bounds for ESP32-C3 patch arena.
pub const MIN_SLOT_COUNT: usize = 2;
pub const MAX_SLOT_COUNT: usize = 4;

/// Known mount roles (`mount` string in config / layout).
pub const MOUNT_CALL: &str = "call";
pub const MOUNT_RX: &str = "rx";

/// Default MCU target fields for the libbpf-mcu contract.
pub const CONTRACT_ARCH: &str = "riscv32";
pub const CONTRACT_ABI: &str = "ilp32";

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

/// Flash (or host write-bin) cold XDP image table from intent `[cold_table]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColdTable {
    /// Absolute flash address for `espflash write-bin` / runtime Flash backend.
    pub base: usize,
    pub length: usize,
    /// Bytes per image cell (stride).
    pub image_capacity: usize,
    /// Optional partition label (informational only).
    pub partition: Option<String>,
}

impl ColdTable {
    pub fn image_count(&self) -> usize {
        if self.image_capacity == 0 {
            return 0;
        }
        self.length / self.image_capacity
    }

    /// Absolute flash address of cell `index`.
    pub fn image_abs(&self, index: usize) -> Option<usize> {
        let off = index.checked_mul(self.image_capacity)?;
        if off.saturating_add(self.image_capacity) > self.length {
            return None;
        }
        self.base.checked_add(off)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub iram_base: usize,
    pub arena_end: usize,
    pub helper: Region,
    pub hot_regions: Vec<HotRegion>,
    /// Optional Flash cold table (hand-path / write-bin).
    pub cold_table: Option<ColdTable>,
    /// Map names from intent `[maps].names` (may be empty).
    pub maps_names: Vec<String>,
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

    /// Index of the unique `rx` mount, else `0`.
    pub fn primary_slot_index(&self) -> usize {
        self.hot_regions
            .iter()
            .position(|r| r.mount == MountKind::Rx)
            .unwrap_or(0)
    }

    pub fn primary_slot(&self) -> Option<&HotRegion> {
        let i = self.primary_slot_index();
        self.hot_regions.get(i)
    }
}

pub struct LayoutReport {
    pub layout: Layout,
    /// Path of the exported contract `layout.toml`, if written.
    pub contract_path: Option<PathBuf>,
    pub lines: Vec<String>,
}

/// Validate board intent `layout.toml` and export the §3.2 contract beside the board
/// under `target/layout-export/layout.toml` (does not overwrite intent).
pub fn doctor(board_dir: &Path) -> Result<LayoutReport> {
    let layout = compute_from_file(&board_dir.join("layout.toml"))?;
    cross_check_firmware_config(board_dir, board_dir)?;
    let maps = resolve_maps_names(board_dir, &layout)?;
    let contract_dir = board_dir.join("target/layout-export");
    fs::create_dir_all(&contract_dir)
        .with_context(|| format!("mkdir {}", contract_dir.display()))?;
    let contract_path = contract_dir.join("layout.toml");
    write_contract_layout_toml(&contract_path, &layout, &maps, CONTRACT_ARCH, CONTRACT_ABI)?;
    let write_bin_path = contract_dir.join("write-bin-addrs.toml");
    write_write_bin_addrs(&write_bin_path, &layout)?;
    let mut lines = report_lines(&layout);
    lines.extend(write_bin_report_lines(&layout));
    Ok(LayoutReport {
        layout,
        contract_path: Some(contract_path),
        lines,
    })
}

/// Export libbpf-mcu contract `layout.toml` to `out_path` from board intent.
pub fn export_contract(board_dir: &Path, out_path: &Path) -> Result<Layout> {
    let layout = compute_from_file(&board_dir.join("layout.toml"))?;
    let maps = resolve_maps_names(board_dir, &layout)?;
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    write_contract_layout_toml(out_path, &layout, &maps, CONTRACT_ARCH, CONTRACT_ABI)?;
    if let Some(parent) = out_path.parent() {
        write_write_bin_addrs(&parent.join("write-bin-addrs.toml"), &layout)?;
    }
    Ok(layout)
}

/// Intent `[maps].names` if non-empty; else scrape `bpf.toml` keys.
pub fn resolve_maps_names(board_dir: &Path, layout: &Layout) -> Result<Vec<String>> {
    if !layout.maps_names.is_empty() {
        return Ok(layout.maps_names.clone());
    }
    map_names_from_bpf_toml(&board_dir.join("bpf.toml"))
}

/// Write §3.2 contract schema (plus `[helper]`, optional `[cold_table]`).
pub fn write_contract_layout_toml(
    path: &Path,
    layout: &Layout,
    maps_names: &[String],
    arch: &str,
    abi: &str,
) -> Result<()> {
    let primary = layout.primary_slot_index();
    let mut out = String::new();
    writeln!(
        out,
        "# @generated by xpost-build after firmware link — do not edit.\n\
         # libbpf-mcu layout contract (metadata + partitions.csv + ELF symbols).\n"
    )
    .unwrap();

    writeln!(out, "[target]").unwrap();
    writeln!(out, "arch = \"{}\"", escape_toml_str(arch)).unwrap();
    writeln!(out, "abi = \"{}\"", escape_toml_str(abi)).unwrap();
    writeln!(out).unwrap();

    // Helper absolute address for AOT `helper_dispatcher` (not in minimal §3.2, but
    // required by libbpf-mcu slot linker scripts).
    writeln!(out, "[helper]").unwrap();
    writeln!(out, "base = {:#x}", layout.helper.base).unwrap();
    writeln!(out, "length = {:#x}", layout.helper.length).unwrap();
    writeln!(out).unwrap();

    for (i, r) in layout.hot_regions.iter().enumerate() {
        writeln!(out, "[[xdp_slots]]").unwrap();
        writeln!(out, "name = \"{}\"", escape_toml_str(&r.name)).unwrap();
        writeln!(out, "base = {:#x}", r.base).unwrap();
        writeln!(out, "length = {:#x}", r.length).unwrap();
        writeln!(out, "primary = {}", i == primary).unwrap();
        if i + 1 < layout.hot_regions.len() {
            writeln!(out).unwrap();
        }
    }
    writeln!(out).unwrap();

    if let Some(c) = &layout.cold_table {
        // Dual-write: legacy [cold_table] + plan [xdp_table].
        writeln!(out, "[cold_table]").unwrap();
        writeln!(out, "base = {:#x}", c.base).unwrap();
        writeln!(out, "length = {:#x}", c.length).unwrap();
        writeln!(out, "image_capacity = {:#x}", c.image_capacity).unwrap();
        if let Some(p) = &c.partition {
            writeln!(out, "partition = \"{}\"", escape_toml_str(p)).unwrap();
        }
        writeln!(out).unwrap();

        writeln!(out, "[xdp_table]").unwrap();
        writeln!(out, "base = {:#x}", c.base).unwrap();
        writeln!(out, "len = {:#x}", c.length).unwrap();
        writeln!(out, "image_capacity = {:#x}", c.image_capacity).unwrap();
        writeln!(out, "image_count = {}", c.image_count()).unwrap();
        if let Some(p) = &c.partition {
            writeln!(out, "partition = \"{}\"", escape_toml_str(p)).unwrap();
        }
        writeln!(out).unwrap();

        // Default-path entity: primary image lives in table cell 0 (inject target).
        writeln!(out, "[[xdp_images]]").unwrap();
        writeln!(out, "name = \"primary\"").unwrap();
        writeln!(out, "table_index = 0").unwrap();
        if let Some(s) = layout.primary_slot() {
            writeln!(out, "link_addr = {:#x}", s.base).unwrap();
        }
        writeln!(out).unwrap();
    }

    writeln!(out, "[maps]").unwrap();
    write!(out, "names = [").unwrap();
    for (i, name) in maps_names.iter().enumerate() {
        if i > 0 {
            write!(out, ", ").unwrap();
        }
        write!(out, "\"{}\"", escape_toml_str(name)).unwrap();
    }
    writeln!(out, "]").unwrap();

    fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Machine-readable Flash write-bin addresses for cold-table cells.
pub fn write_write_bin_addrs(path: &Path, layout: &Layout) -> Result<()> {
    let mut out = String::new();
    writeln!(
        out,
        "# @generated — packed Compact ELF region for `espflash write-bin <addr> <xdp.elf>`\n"
    )
    .unwrap();
    match &layout.cold_table {
        None => {
            writeln!(
                out,
                "# [cold_table] absent in intent layout.toml — no write-bin targets."
            )
            .unwrap();
        }
        Some(c) => {
            writeln!(out, "cold_table_flash_base = {:#x}", c.base).unwrap();
            writeln!(out, "cold_table_flash_len = {:#x}", c.length).unwrap();
            writeln!(out, "image_capacity = {:#x}", c.image_capacity).unwrap();
            writeln!(out, "image_count = {}", c.image_count()).unwrap();
            writeln!(out).unwrap();
            writeln!(
                out,
                "# Packed Compact ELF (variable-length) starts at cold_table_flash_base."
            )
            .unwrap();
            writeln!(out, "#   espflash write-bin {:#x} <xdp.elf>", c.base).unwrap();
            writeln!(out).unwrap();
            for i in 0..c.image_count() {
                if let Some(abs) = c.image_abs(i) {
                    writeln!(out, "image{i}_abs = {abs:#x}").unwrap();
                }
            }
            writeln!(out).unwrap();
            writeln!(out, "helper_dispatcher = {:#x}", layout.helper.base).unwrap();
            if let Some(s) = layout.primary_slot() {
                writeln!(out, "primary_slot = {:#x}", s.base).unwrap();
                writeln!(out, "primary_slot_len = {:#x}", s.length).unwrap();
            }
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn write_bin_report_lines(layout: &Layout) -> Vec<String> {
    let mut lines = Vec::new();
    match &layout.cold_table {
        None => lines.push("cold_table: (none)".into()),
        Some(c) => {
            lines.push(format!(
                "cold_table base={:#x} len={:#x} stride={:#x} cells={}",
                c.base,
                c.length,
                c.image_capacity,
                c.image_count()
            ));
            if let Some(abs0) = c.image_abs(0) {
                lines.push(format!(
                    "write-bin example: espflash write-bin {abs0:#x} <xdp.elf>"
                ));
            }
        }
    }
    lines
}

/// Patch contract + write-bin addrs using linked ELF symbol addresses.
///
/// Updates `helper.base` from `helper_dispatcher`. Slot bases stay firmware-owned.
/// Cold flash comes from `partitions.csv` via [`crate::metadata::resolve_layout`].
pub fn patch_contract_from_elf(
    crate_manifest: &Path,
    board_dir: &Path,
    contract_path: &Path,
    elf_path: &Path,
) -> Result<()> {
    let crate_meta = crate::metadata::load_from_cargo_toml(crate_manifest)?
        .unwrap_or_else(crate::metadata::XpostMetadata::default_esp32c3);
    let (_meta, mut layout) = crate::metadata::resolve_layout(&crate_meta, Some(board_dir))?;
    layout.helper.base = elf_symbol_addr(elf_path, "helper_dispatcher")?;
    let maps = resolve_maps_names(board_dir, &layout)?;
    write_contract_layout_toml(contract_path, &layout, &maps, CONTRACT_ARCH, CONTRACT_ABI)?;
    if let Some(parent) = contract_path.parent() {
        write_write_bin_addrs(&parent.join("write-bin-addrs.toml"), &layout)?;
    }
    Ok(())
}

/// Read a global symbol address from an ELF.
pub fn elf_symbol_addr(elf_path: &Path, name: &str) -> Result<usize> {
    use object::{Object, ObjectSymbol};

    let data = fs::read(elf_path).with_context(|| format!("read ELF {}", elf_path.display()))?;
    let file = object::File::parse(&*data).context("parse ELF")?;
    for sym in file.symbols() {
        if sym.name().ok() == Some(name) {
            return Ok(sym.address() as usize);
        }
    }
    for sym in file.dynamic_symbols() {
        if sym.name().ok() == Some(name) {
            return Ok(sym.address() as usize);
        }
    }
    bail!("ELF {}: symbol `{name}` not found", elf_path.display())
}

/// Map section names from `bpf.toml` `[maps.*]` keys (empty if missing).
pub fn map_names_from_bpf_toml(path: &Path) -> Result<Vec<String>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let root: toml::Table = text
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let Some(maps) = root.get("maps").and_then(|v| v.as_table()) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = maps.keys().map(|k| k.to_string()).collect();
    names.sort();
    Ok(names)
}

fn escape_toml_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
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
        cold_table: parse_cold_table(root)?,
        maps_names: parse_maps_names(root)?,
    };
    validate_abut(&layout)?;
    Ok(layout)
}

fn parse_cold_table(root: &toml::Table) -> Result<Option<ColdTable>> {
    let Some(t) = root.get("cold_table").and_then(|v| v.as_table()) else {
        return Ok(None);
    };
    for (key, _) in t {
        if !matches!(
            key.as_str(),
            "base" | "length" | "image_capacity" | "partition"
        ) {
            bail!("layout: [cold_table] unknown key `{key}`");
        }
    }
    let base = parse_usize_field(t, "base", "cold_table")?;
    let length = parse_usize_field(t, "length", "cold_table")?;
    let image_capacity = parse_usize_field(t, "image_capacity", "cold_table")?;
    if length == 0 || image_capacity == 0 {
        bail!("[cold_table] length and image_capacity must be > 0");
    }
    if !base.is_multiple_of(4) || !length.is_multiple_of(4) || !image_capacity.is_multiple_of(4) {
        bail!("[cold_table] base/length/image_capacity must be 4-byte aligned");
    }
    if length % image_capacity != 0 {
        bail!(
            "[cold_table] length {length:#x} must be a multiple of image_capacity {image_capacity:#x}"
        );
    }
    let partition = match t.get("partition") {
        None => None,
        Some(v) => Some(
            v.as_str()
                .ok_or_else(|| anyhow::anyhow!("layout: [cold_table].partition must be a string"))?
                .to_string(),
        ),
    };
    Ok(Some(ColdTable {
        base,
        length,
        image_capacity,
        partition,
    }))
}

fn parse_maps_names(root: &toml::Table) -> Result<Vec<String>> {
    let Some(maps) = root.get("maps").and_then(|v| v.as_table()) else {
        return Ok(Vec::new());
    };
    let Some(names_v) = maps.get("names") else {
        return Ok(Vec::new());
    };
    let arr = names_v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("layout: [maps].names must be an array of strings"))?;
    let mut names = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let s = v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("layout: [maps].names[{i}] must be a string"))?;
        if s.is_empty() {
            bail!("layout: [maps].names[{i}] must be non-empty");
        }
        names.push(s.to_string());
    }
    Ok(names)
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

/// If board `config.toml` still has `[arena]`/`[helper]`/`[slots]`, they must
/// match `layout.toml`.
pub fn cross_check_firmware_config(board_dir: &Path, layout_dir: &Path) -> Result<()> {
    let path = board_dir.join("config.toml");
    if !path.is_file() {
        return Ok(());
    }
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
    let iram_eq = from_cfg.iram_base == from_layout.iram_base
        && from_cfg.arena_end == from_layout.arena_end
        && from_cfg.helper == from_layout.helper
        && from_cfg.hot_regions == from_layout.hot_regions;
    if !iram_eq {
        bail!(
            "config.toml [arena]/[helper]/[slots] disagrees with layout.toml\n\
             layout lives beside the example; remove the config.toml copy or make them match"
        );
    }
    Ok(())
}

pub fn report_lines(layout: &Layout) -> Vec<String> {
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
    if !layout.maps_names.is_empty() {
        lines.push(format!("maps names={:?}", layout.maps_names));
    }
    lines.push(format!(
        "used={:#x} remaining={:#x}",
        layout.used_len(),
        layout.remaining()
    ));
    lines
}
