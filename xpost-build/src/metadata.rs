//! `[package.metadata.xpost]` schema + merge with board intent `layout.toml`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::layout::{ColdTable, HotRegion, Layout, MountKind, Region, compute_from_file};

/// Schema version understood by this crate.
pub const METADATA_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotEntry {
    pub name: String,
    pub length: usize,
    pub mount: MountKind,
    pub primary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdpTableMeta {
    pub flash_base: usize,
    pub flash_len: usize,
    pub image_capacity: usize,
    pub image_count: Option<usize>,
    pub partition: Option<String>,
}

impl XdpTableMeta {
    pub fn resolved_image_count(&self) -> usize {
        if let Some(n) = self.image_count {
            return n;
        }
        if self.image_capacity == 0 {
            return 0;
        }
        self.flash_len / self.image_capacity
    }

    pub fn to_cold_table(&self) -> ColdTable {
        ColdTable {
            base: self.flash_base,
            length: self.flash_len,
            image_capacity: self.image_capacity,
            partition: self.partition.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetMeta {
    pub arch: String,
    pub abi: String,
    pub iram_base: Option<usize>,
    pub arena_end: Option<usize>,
    pub helper_length: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XpostMetadata {
    pub schema_version: u32,
    pub map_heap_size: usize,
    pub slots: Vec<SlotEntry>,
    pub xdp_table: Option<XdpTableMeta>,
    pub target: TargetMeta,
    pub maps_names: Vec<String>,
}

impl XpostMetadata {
    /// Defaults matching historical ESP32-C3 direct example sizes.
    pub fn default_esp32c3() -> Self {
        Self {
            schema_version: METADATA_SCHEMA_VERSION,
            map_heap_size: crate::HEAP_SIZE,
            slots: vec![
                SlotEntry {
                    name: "a".into(),
                    length: 0xE00,
                    mount: MountKind::Rx,
                    primary: true,
                },
                SlotEntry {
                    name: "b".into(),
                    length: 0x1000,
                    mount: MountKind::Call,
                    primary: false,
                },
            ],
            xdp_table: Some(XdpTableMeta {
                flash_base: 0,
                flash_len: 2 * 4096,
                image_capacity: 4096,
                image_count: Some(2),
                partition: None,
            }),
            target: TargetMeta {
                arch: crate::layout::CONTRACT_ARCH.into(),
                abi: crate::layout::CONTRACT_ABI.into(),
                iram_base: Some(0x40380000),
                arena_end: Some(0x40382000),
                helper_length: Some(0x200),
            },
            maps_names: Vec::new(),
        }
    }
}

/// Parse `[package.metadata.xpost]` from a Cargo.toml document root.
pub fn parse_cargo_metadata_xpost(cargo_toml: &toml::Table) -> Result<Option<XpostMetadata>> {
    let Some(meta) = cargo_toml
        .get("package")
        .and_then(|v| v.as_table())
        .and_then(|p| p.get("metadata"))
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("xpost"))
        .and_then(|v| v.as_table())
    else {
        return Ok(None);
    };
    Ok(Some(parse_xpost_table(meta)?))
}

/// Load metadata from a crate `Cargo.toml` path.
pub fn load_from_cargo_toml(path: &Path) -> Result<Option<XpostMetadata>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let root: toml::Table = text
        .parse()
        .with_context(|| format!("parse {}", path.display()))?;
    parse_cargo_metadata_xpost(&root)
}

fn parse_xpost_table(meta: &toml::Table) -> Result<XpostMetadata> {
    let schema_version = meta
        .get("schema_version")
        .and_then(|v| v.as_integer())
        .unwrap_or(METADATA_SCHEMA_VERSION as i64) as u32;
    if schema_version > METADATA_SCHEMA_VERSION {
        bail!(
            "metadata.xpost schema_version {schema_version} > supported {METADATA_SCHEMA_VERSION}"
        );
    }

    let map_heap_size = meta
        .get("map")
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("heap_size"))
        .and_then(|v| v.as_integer())
        .map(|n| n as usize)
        .unwrap_or(crate::HEAP_SIZE);

    let slots = parse_slots(meta)?;
    let xdp_table = parse_xdp_table(meta)?;
    let target = parse_target(meta)?;
    let maps_names = meta
        .get("maps")
        .and_then(|v| v.as_table())
        .and_then(|m| m.get("names"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok(XpostMetadata {
        schema_version,
        map_heap_size,
        slots,
        xdp_table,
        target,
        maps_names,
    })
}

fn parse_slots(meta: &toml::Table) -> Result<Vec<SlotEntry>> {
    if let Some(entries) = meta
        .get("slots")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("entries"))
        .and_then(|v| v.as_array())
    {
        let mut out = Vec::new();
        for (i, ent) in entries.iter().enumerate() {
            let t = ent
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("slots.entries[{i}] must be a table"))?;
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("slots.entries[{i}] missing name"))?
                .to_string();
            let length = t
                .get("length")
                .and_then(|v| v.as_integer())
                .ok_or_else(|| anyhow::anyhow!("slots.entries[{i}] missing length"))?
                as usize;
            let mount = match t.get("mount").and_then(|v| v.as_str()) {
                Some(s) => MountKind::parse(s)?,
                None => MountKind::Call,
            };
            let primary = t
                .get("primary")
                .and_then(|v| v.as_bool())
                .unwrap_or(mount == MountKind::Rx);
            out.push(SlotEntry {
                name,
                length,
                mount,
                primary,
            });
        }
        if out.is_empty() {
            bail!("slots.entries must not be empty");
        }
        return Ok(out);
    }

    // Flat form: count + length (+ optional mounts left as rx/call default).
    let slots_t = meta.get("slots").and_then(|v| v.as_table());
    if let Some(s) = slots_t {
        if let (Some(count), Some(length)) = (
            s.get("count").and_then(|v| v.as_integer()),
            s.get("length").and_then(|v| v.as_integer()),
        ) {
            let count = count as usize;
            let length = length as usize;
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let primary = i == 0;
                out.push(SlotEntry {
                    name: format!("slot{i}"),
                    length,
                    mount: if primary {
                        MountKind::Rx
                    } else {
                        MountKind::Call
                    },
                    primary,
                });
            }
            return Ok(out);
        }
    }

    Ok(XpostMetadata::default_esp32c3().slots)
}

fn parse_xdp_table(meta: &toml::Table) -> Result<Option<XdpTableMeta>> {
    let Some(t) = meta.get("xdp_table").and_then(|v| v.as_table()) else {
        return Ok(XpostMetadata::default_esp32c3().xdp_table);
    };
    let flash_base = t
        .get("flash_base")
        .and_then(|v| v.as_integer())
        .unwrap_or(0) as usize;
    let flash_len = t
        .get("flash_len")
        .and_then(|v| v.as_integer())
        .ok_or_else(|| anyhow::anyhow!("xdp_table.flash_len required"))? as usize;
    let image_capacity = t
        .get("image_capacity")
        .and_then(|v| v.as_integer())
        .ok_or_else(|| anyhow::anyhow!("xdp_table.image_capacity required"))?
        as usize;
    let image_count = t
        .get("image_count")
        .and_then(|v| v.as_integer())
        .map(|n| n as usize);
    let partition = t
        .get("partition")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(Some(XdpTableMeta {
        flash_base,
        flash_len,
        image_capacity,
        image_count,
        partition,
    }))
}

fn parse_target(meta: &toml::Table) -> Result<TargetMeta> {
    let def = XpostMetadata::default_esp32c3().target;
    let Some(t) = meta.get("target").and_then(|v| v.as_table()) else {
        return Ok(def);
    };
    Ok(TargetMeta {
        arch: t
            .get("arch")
            .and_then(|v| v.as_str())
            .unwrap_or(&def.arch)
            .to_string(),
        abi: t
            .get("abi")
            .and_then(|v| v.as_str())
            .unwrap_or(&def.abi)
            .to_string(),
        iram_base: t
            .get("iram_base")
            .and_then(|v| v.as_integer())
            .map(|n| n as usize)
            .or(def.iram_base),
        arena_end: t
            .get("arena_end")
            .and_then(|v| v.as_integer())
            .map(|n| n as usize)
            .or(def.arena_end),
        helper_length: t
            .get("helper_length")
            .and_then(|v| v.as_integer())
            .map(|n| n as usize)
            .or(def.helper_length),
    })
}

/// Merge crate metadata with optional board override metadata and/or intent `layout.toml`.
///
/// Precedence: board Cargo.toml metadata > crate metadata > intent layout.toml for IRAM.
pub fn resolve_layout(
    crate_meta: &XpostMetadata,
    board_dir: Option<&Path>,
) -> Result<(XpostMetadata, Layout)> {
    let mut meta = crate_meta.clone();
    if let Some(board) = board_dir {
        let board_cargo = board.join("Cargo.toml");
        if board_cargo.is_file() {
            if let Some(over) = load_from_cargo_toml(&board_cargo)? {
                merge_metadata(&mut meta, &over);
            }
        }
        let intent = board.join("layout.toml");
        if intent.is_file() {
            let layout = compute_from_file(&intent)?;
            let meta = meta_from_layout_overlay(meta, &layout);
            let layout = layout_with_meta(&layout, &meta);
            return Ok((meta, layout));
        }
    }
    let layout = layout_from_metadata(&meta)?;
    Ok((meta, layout))
}

fn merge_metadata(base: &mut XpostMetadata, over: &XpostMetadata) {
    base.schema_version = over.schema_version;
    if over.map_heap_size != 0 {
        base.map_heap_size = over.map_heap_size;
    }
    if !over.slots.is_empty() {
        base.slots = over.slots.clone();
    }
    if over.xdp_table.is_some() {
        base.xdp_table = over.xdp_table.clone();
    }
    if over.target.iram_base.is_some() {
        base.target.iram_base = over.target.iram_base;
    }
    if over.target.arena_end.is_some() {
        base.target.arena_end = over.target.arena_end;
    }
    if over.target.helper_length.is_some() {
        base.target.helper_length = over.target.helper_length;
    }
    if !over.target.arch.is_empty() {
        base.target.arch = over.target.arch.clone();
    }
    if !over.target.abi.is_empty() {
        base.target.abi = over.target.abi.clone();
    }
    if !over.maps_names.is_empty() {
        base.maps_names = over.maps_names.clone();
    }
}

fn meta_from_layout_overlay(mut meta: XpostMetadata, layout: &Layout) -> XpostMetadata {
    meta.target.iram_base = Some(layout.iram_base);
    meta.target.arena_end = Some(layout.arena_end);
    meta.target.helper_length = Some(layout.helper.length);
    if meta.slots.is_empty() {
        let primary = layout.primary_slot_index();
        meta.slots = layout
            .hot_regions
            .iter()
            .enumerate()
            .map(|(i, r)| SlotEntry {
                name: r.name.clone(),
                length: r.length,
                mount: r.mount,
                primary: i == primary,
            })
            .collect();
    }
    if meta.xdp_table.is_none() {
        meta.xdp_table = layout.cold_table.as_ref().map(|c| XdpTableMeta {
            flash_base: c.base,
            flash_len: c.length,
            image_capacity: c.image_capacity,
            image_count: Some(c.image_count()),
            partition: c.partition.clone(),
        });
    }
    if meta.maps_names.is_empty() {
        meta.maps_names = layout.maps_names.clone();
    }
    meta
}

fn layout_with_meta(layout: &Layout, meta: &XpostMetadata) -> Layout {
    let mut out = layout.clone();
    if let Some(t) = &meta.xdp_table {
        out.cold_table = Some(t.to_cold_table());
    }
    if !meta.maps_names.is_empty() {
        out.maps_names = meta.maps_names.clone();
    }
    out
}

fn layout_from_metadata(meta: &XpostMetadata) -> Result<Layout> {
    let iram_base = meta
        .target
        .iram_base
        .ok_or_else(|| anyhow::anyhow!("metadata.xpost.target.iram_base required without layout.toml"))?;
    let arena_end = meta
        .target
        .arena_end
        .ok_or_else(|| anyhow::anyhow!("metadata.xpost.target.arena_end required without layout.toml"))?;
    let helper_len = meta
        .target
        .helper_length
        .ok_or_else(|| anyhow::anyhow!("metadata.xpost.target.helper_length required without layout.toml"))?;

    if meta.slots.is_empty() {
        bail!("metadata.xpost.slots empty");
    }
    let helper = Region {
        base: iram_base,
        length: helper_len,
    };
    let mut cursor = iram_base + helper_len;
    let mut hot_regions = Vec::with_capacity(meta.slots.len());
    for s in &meta.slots {
        hot_regions.push(HotRegion {
            name: s.name.clone(),
            base: cursor,
            length: s.length,
            mount: s.mount,
        });
        cursor += s.length;
    }
    Ok(Layout {
        iram_base,
        arena_end,
        helper,
        hot_regions,
        cold_table: meta.xdp_table.as_ref().map(|t| t.to_cold_table()),
        maps_names: meta.maps_names.clone(),
    })
}
