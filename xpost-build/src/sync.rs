//! Unified sync entry: validate / codegen / embed_layout / export / patch-elf / write-bin.
//!
//! Path A default endpoint is [`embed_layout_sync`] (`.xpost.mem` / XPM1).
//! Contract `layout.toml` export remains available but is no longer the main contract.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::layout::{self, Layout, LayoutReport, patch_contract_from_elf, write_write_bin_addrs};
use crate::metadata::{self, XpostMetadata};
use crate::{RuntimeGen, generate_runtime, mem_map};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncMode {
    /// Default path A: metadata + partitions; post-link embeds `.xpost.mem`.
    Default,
    /// Path B: `bpf.toml` + bin → boot arrays; no contract export.
    FromBpfToml,
}

pub struct SyncRequest<'a> {
    pub mode: SyncMode,
    /// Crate providing `[package.metadata.xpost]` (usually xpost root).
    pub crate_manifest: &'a Path,
    /// Board dir (`partitions.csv` / optional `bpf.toml`). Optional for metadata-only.
    pub board_dir: Option<&'a Path>,
    pub out_dir: &'a Path,
    /// Where to write contract (path A). Default: `board_dir/target/layout-export/layout.toml`.
    pub contract_out: Option<&'a Path>,
    pub elf_path: Option<&'a Path>,
}

pub struct SyncReport {
    pub meta: XpostMetadata,
    pub layout: Layout,
    pub custom_sections_path: Option<PathBuf>,
    pub contract_path: Option<PathBuf>,
    pub warnings: Vec<String>,
    pub lines: Vec<String>,
}

/// Validate metadata (+ optional board intent) without writing artifacts.
pub fn validate(req: &SyncRequest<'_>) -> Result<SyncReport> {
    let (meta, layout) = resolve(req)?;
    let mut lines = layout::report_lines(&layout);
    lines.extend(layout::write_bin_report_lines(&layout));
    Ok(SyncReport {
        meta,
        layout,
        custom_sections_path: None,
        contract_path: None,
        warnings: Vec::new(),
        lines,
    })
}

/// Codegen runtime artifacts into `out_dir` (mem_map / hot_slots / linker; B adds bpf tables).
pub fn codegen(req: &SyncRequest<'_>) -> Result<SyncReport> {
    let (meta, layout) = resolve(req)?;
    fs::create_dir_all(req.out_dir).with_context(|| format!("mkdir {}", req.out_dir.display()))?;

    write_runtime_gen(&meta, req.out_dir)?;

    let rt = match req.mode {
        SyncMode::Default => {
            let mem = mem_map::generate(&layout, req.out_dir)?;
            write_empty_bpf_boot_arrays(req.out_dir)?;
            RuntimeGen {
                custom_sections_path: mem.custom_sections_path,
                warnings: Vec::new(),
            }
        }
        SyncMode::FromBpfToml => {
            let Some(board) = req.board_dir else {
                bail!("from_bpf_toml sync requires board_dir with bpf.toml");
            };
            let bpf = board.join("bpf.toml");
            if !bpf.is_file() {
                bail!("from_bpf_toml requires {}", bpf.display());
            }
            generate_runtime(board, req.out_dir)?
        }
    };

    Ok(SyncReport {
        meta,
        layout,
        custom_sections_path: Some(rt.custom_sections_path),
        contract_path: None,
        warnings: rt.warnings,
        lines: Vec::new(),
    })
}

/// Export contract `layout.toml` + write-bin addrs (path A only).
pub fn export(req: &SyncRequest<'_>) -> Result<SyncReport> {
    if req.mode != SyncMode::Default {
        bail!("export_contract is only valid for SyncMode::Default (path A)");
    }
    let (meta, layout) = resolve(req)?;
    let contract_path = contract_path_for(req)?;
    if let Some(parent) = contract_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let maps = if meta.maps_names.is_empty() {
        if let Some(board) = req.board_dir {
            layout::resolve_maps_names(board, &layout)?
        } else {
            meta.maps_names.clone()
        }
    } else {
        meta.maps_names.clone()
    };
    layout::write_contract_layout_toml(
        &contract_path,
        &layout,
        &maps,
        &meta.target.arch,
        &meta.target.abi,
    )?;
    if let Some(parent) = contract_path.parent() {
        write_write_bin_addrs(&parent.join("write-bin-addrs.toml"), &layout)?;
    }
    let mut lines = layout::report_lines(&layout);
    lines.extend(layout::write_bin_report_lines(&layout));
    Ok(SyncReport {
        meta,
        layout,
        custom_sections_path: None,
        contract_path: Some(contract_path),
        warnings: Vec::new(),
        lines,
    })
}

/// Patch contract addresses from a linked ELF (path A).
pub fn patch_elf(req: &SyncRequest<'_>) -> Result<SyncReport> {
    if req.mode != SyncMode::Default {
        bail!("patch_elf is only valid for SyncMode::Default");
    }
    let Some(elf) = req.elf_path else {
        bail!("patch_elf requires elf_path");
    };
    let Some(board) = req.board_dir else {
        bail!("patch_elf requires board_dir (intent layout for slot index)");
    };
    let contract_path = contract_path_for(req)?;
    patch_contract_from_elf(req.crate_manifest, board, &contract_path, elf)?;
    let (meta, layout) = resolve(req)?;
    Ok(SyncReport {
        meta,
        layout,
        custom_sections_path: None,
        contract_path: Some(contract_path),
        warnings: Vec::new(),
        lines: Vec::new(),
    })
}

/// Write-bin address report only.
pub fn write_bin(req: &SyncRequest<'_>) -> Result<SyncReport> {
    let (meta, layout) = resolve(req)?;
    let out = match req.contract_out {
        Some(p) => p
            .parent()
            .map(|d| d.join("write-bin-addrs.toml"))
            .unwrap_or_else(|| PathBuf::from("write-bin-addrs.toml")),
        None => match req.board_dir {
            Some(b) => b.join("target/layout-export/write-bin-addrs.toml"),
            None => req.out_dir.join("write-bin-addrs.toml"),
        },
    };
    write_write_bin_addrs(&out, &layout)?;
    let lines = layout::write_bin_report_lines(&layout);
    Ok(SyncReport {
        meta,
        layout,
        custom_sections_path: None,
        contract_path: None,
        warnings: Vec::new(),
        lines,
    })
}

/// Full path-A sync: codegen, then embed `.xpost.mem` when `elf_path` is set.
pub fn sync_default(req: &SyncRequest<'_>) -> Result<SyncReport> {
    let mut report = codegen(req)?;
    if req.elf_path.is_some() {
        let emb = embed_layout_sync(req)?;
        report.lines = emb.lines;
        report.warnings.extend(emb.warnings);
    }
    Ok(report)
}

/// Embed XPM1 `.xpost.mem` into the linked firmware ELF (path A endpoint).
pub fn embed_layout_sync(req: &SyncRequest<'_>) -> Result<SyncReport> {
    if req.mode != SyncMode::Default {
        bail!("embed_layout is only valid for SyncMode::Default");
    }
    let Some(elf) = req.elf_path else {
        bail!("embed_layout requires elf_path");
    };
    let Some(board) = req.board_dir else {
        bail!("embed_layout requires board_dir (partitions.csv)");
    };
    let map = crate::embed::embed_layout(req.crate_manifest, board, elf)?;
    let (meta, layout) = resolve(req)?;
    let lines = vec![
        format!(
            ".xpost.mem helper={:#x} slot={:#x}+{:#x} table={:#x}+{:#x}",
            map.helper_addr, map.slot_addr, map.slot_len, map.table_addr, map.table_len
        ),
        format!("embedded into {}", elf.display()),
    ];
    Ok(SyncReport {
        meta,
        layout,
        custom_sections_path: None,
        contract_path: None,
        warnings: Vec::new(),
        lines,
    })
}

/// Full path-B sync: codegen boot arrays only (no mem embed).
pub fn sync_from_bpf_toml(req: &SyncRequest<'_>) -> Result<SyncReport> {
    let r = SyncRequest {
        mode: SyncMode::FromBpfToml,
        crate_manifest: req.crate_manifest,
        board_dir: req.board_dir,
        out_dir: req.out_dir,
        contract_out: req.contract_out,
        elf_path: req.elf_path,
    };
    codegen(&r)
}

fn resolve(req: &SyncRequest<'_>) -> Result<(XpostMetadata, Layout)> {
    let crate_meta = metadata::load_from_cargo_toml(req.crate_manifest)?
        .unwrap_or_else(XpostMetadata::default_esp32c3);
    metadata::resolve_layout(&crate_meta, req.board_dir)
}

fn contract_path_for(req: &SyncRequest<'_>) -> Result<PathBuf> {
    if let Some(p) = req.contract_out {
        return Ok(p.to_path_buf());
    }
    let Some(board) = req.board_dir else {
        bail!("contract export needs board_dir or contract_out");
    };
    Ok(board.join("target/layout-export/layout.toml"))
}

fn write_runtime_gen(meta: &XpostMetadata, out_dir: &Path) -> Result<()> {
    let mut out = String::from("// @generated by xpost-build sync — declare_runtime! input\n");
    out.push_str(&format!(
        "pub const MAP_HEAP_SIZE: usize = {};\n",
        meta.map_heap_size
    ));
    out.push_str(&format!(
        "pub const XDP_SLOT_COUNT: usize = {};\n",
        meta.slots.len()
    ));
    let max_len = meta.slots.iter().map(|s| s.length).max().unwrap_or(0);
    out.push_str(&format!(
        "pub const XDP_SLOT_LEN_MAX: usize = {max_len:#x};\n"
    ));
    out.push_str("pub const XDP_SLOT_LENS: &[usize] = &[");
    for (i, s) in meta.slots.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{:#x}", s.length));
    }
    out.push_str("];\n");
    out.push_str("pub const XDP_SLOT_PRIMARY: &[bool] = &[");
    for (i, s) in meta.slots.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(if s.primary { "true" } else { "false" });
    }
    out.push_str("];\n");

    let (table_len, image_cap, flash_base) = match &meta.xdp_table {
        Some(t) => (t.flash_len, t.image_capacity, t.flash_base),
        None => (2 * 4096, 4096, 0usize),
    };
    out.push_str(&format!(
        "pub const XDP_TABLE_BYTES: usize = {table_len:#x};\n"
    ));
    out.push_str(&format!(
        "pub const XDP_TABLE_IMAGE_CAPACITY: usize = {image_cap:#x};\n"
    ));
    out.push_str(&format!(
        "pub const XDP_TABLE_FLASH_BASE: usize = {flash_base:#x};\n"
    ));

    fs::write(out_dir.join("xpost_runtime_gen.rs"), out).context("write xpost_runtime_gen.rs")?;
    Ok(())
}

fn write_empty_bpf_boot_arrays(out_dir: &Path) -> Result<()> {
    fs::write(
        out_dir.join("bpf_maps.rs"),
        r#"// empty — path A
pub const FIRMWARE_MAP_COUNT: usize = 0;
pub fn boot_maps(_rt: &mut ::xpost::map::MapRt) -> Result<(), ::xpost::map::Error> { Ok(()) }
"#,
    )?;
    fs::write(
        out_dir.join("xdp_bpf.rs"),
        "pub fn boot_xdps(_rt: &mut ::xpost::xdp::XdpRt) -> Result<(), ::xpost::xdp::XdpError> { Ok(()) }\n",
    )?;
    fs::write(
        out_dir.join("aot_map_bind.rs"),
        r#"pub fn bind_aot_map_addrs() {}
pub const AOT_MAP_BIND_COUNT: usize = 0;
pub const AOT_MAP_BINDS: &[(&str, usize)] = &[];
pub const AOT_MAP_USER_NAMES: &[&str] = &[];
"#,
    )?;
    Ok(())
}

/// Convenience: validate + export using metadata ∪ board intent.
pub fn doctor_with_metadata(crate_manifest: &Path, board_dir: &Path) -> Result<LayoutReport> {
    let out = board_dir.join("target/layout-export");
    let req = SyncRequest {
        mode: SyncMode::Default,
        crate_manifest,
        board_dir: Some(board_dir),
        out_dir: &out,
        contract_out: None,
        elf_path: None,
    };
    let report = export(&req)?;
    Ok(LayoutReport {
        layout: report.layout,
        contract_path: report.contract_path,
        lines: report.lines,
    })
}
