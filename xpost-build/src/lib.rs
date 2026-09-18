//! Board codegen library — metadata / partitions.csv → Rust + helper IRAM linker script.
//!
//! Prefer [`sync`] as the unified entry (`validate` / `codegen` / `embed_layout` /
//! `write_bin`). Default path: cold flash from `partitions.csv`; after link,
//! [`embed::embed_layout`] writes `.xpost.mem` (XPM1) into the firmware ELF.
//! Crate sizes come from `[package.metadata.xpost]`.
//!
//! `mem.toml` is obsolete; codegen consumes in-memory [`Layout`].

mod aot_bind;
mod bpf_maps;
mod bpf_xdp;
mod config;
pub mod embed;
mod layout;
mod map_view;
mod mem_map;
pub mod metadata;
pub mod partitions;
pub mod sync;
mod util;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use embed::{HELPER_SYMBOL, SLOT_SYMBOL, embed_layout, read_mem_section};
pub use layout::{
    CONTRACT_ABI, CONTRACT_ARCH, ColdTable, Layout, LayoutReport, MAX_SLOT_COUNT, MIN_SLOT_COUNT,
    MountKind, elf_symbol_addr, export_contract, map_names_from_bpf_toml, patch_contract_from_elf,
    report_lines, resolve_maps_names, write_bin_report_lines, write_contract_layout_toml,
    write_write_bin_addrs,
};
pub use map_view::{BusType, RenderRow, Section};
pub use metadata::{
    METADATA_SCHEMA_VERSION, SlotEntry, TargetMeta, XdpTableMeta, XpostMetadata,
    load_from_cargo_toml, resolve_layout,
};
pub use partitions::{
    PartitionRow, fill_xdp_table_from_csv, parse_partitions_csv, validate_xdp_table_against_partitions,
};
pub use sync::{
    SyncMode, SyncReport, SyncRequest, codegen, doctor_with_metadata, embed_layout_sync, export,
    patch_elf, sync_default, sync_from_bpf_toml, validate, write_bin,
};

/// Resolve board dir: `XPOST_BOARD` env, else `manifest_dir` (example package root).
pub fn board_dir(manifest_or_hint: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("XPOST_BOARD") {
        return PathBuf::from(p);
    }
    manifest_or_hint.to_path_buf()
}

/// Validate intent `layout.toml` and export the §3.2 contract under
/// `board_dir/target/layout-export/layout.toml`.
pub fn doctor(board_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(board_dir)
}

/// Deprecated alias for [`doctor`].
pub fn layout_apply(board_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(board_dir)
}

/// Print IBUS/DBUS dual-map SRAM diagram from intent `layout.toml`.
pub fn map_print(board_dir: &Path) -> Result<()> {
    map_view::run_map(board_dir)
}

/// Must match `HEAP_SIZE` in `xpost` map pool / `[package.metadata.xpost.map].heap_size`.
pub const HEAP_SIZE: usize = 65536;
/// Reserved for talc metadata + registry HashMap overhead inside the pool.
pub const POOL_RESERVED: usize = 2048;

/// Highest `schema_version` this xtask understands.
pub const MAX_CONFIG_SCHEMA: i64 = 1;

/// Userspace cfgs derived from enabled `[[tests]]`.
pub const US_CFGS: &[&str] = &[
    "vip_seed",
    "hot_load",
    "firewall_seed",
    "fw_rules_seed",
    "firewall_array_seed",
    "xdp_rx",
    "hw_selftest",
    "map_crud",
    "cold_boot",
];

/// Known `[[tests]].name` values (router-probe registry alignment).
pub const KNOWN_TESTS: &[&str] = &[
    "firewall_echo",
    "xdp_forward_echo",
    "hot_load_ctrl",
    "fw_hash_test",
    "drop_xdp",
    "tail_call_abc",
    "hw_selftest",
    "test0",
];

pub struct RuntimeGen {
    pub custom_sections_path: PathBuf,
    pub warnings: Vec<String>,
}

pub struct FwCfgGen {
    pub enabled_us_cfgs: Vec<String>,
    pub warnings: Vec<String>,
}

pub struct GenResult {
    pub enabled_us_cfgs: Vec<String>,
    pub custom_sections_path: PathBuf,
    pub warnings: Vec<String>,
}

/// Generate runtime artifacts (`mem_map.rs`, `hot_slots.rs`, IRAM linker script,
/// `bpf_maps.rs`, `xdp_bpf.rs`, `aot_map_bind.rs`) from tomls in `board_dir`.
pub fn generate_runtime(board_dir: &Path, out_dir: &Path) -> Result<RuntimeGen> {
    let mem = generate_layout_runtime(board_dir, out_dir)?;
    bpf_maps::generate(board_dir, out_dir)?;
    bpf_xdp::generate(board_dir, out_dir)?;
    let aot = aot_bind::generate(board_dir, out_dir)?;
    let mut warnings = mem.warnings;
    warnings.extend(aot.warnings);
    Ok(RuntimeGen {
        custom_sections_path: mem.custom_sections_path,
        warnings,
    })
}

/// Generate layout artifacts only (`mem_map.rs`, `hot_slots.rs`, linker fragment).
///
/// Skips bpf.toml boot tables (`bpf_maps` / `xdp_bpf` / `aot_map_bind`). Callers that
/// need the direct-start path should write empty stubs for those files themselves.
pub fn generate_layout_runtime(board_dir: &Path, out_dir: &Path) -> Result<RuntimeGen> {
    let layout = layout::compute_from_file(&board_dir.join("layout.toml"))?;
    layout::cross_check_firmware_config(board_dir, board_dir)?;
    if std::env::var_os("XTASK_PRINT_MAP").is_some() {
        let _ = map_view::run_map(board_dir);
    }
    let mem = mem_map::generate(&layout, out_dir)?;
    Ok(RuntimeGen {
        custom_sections_path: mem.custom_sections_path,
        warnings: Vec::new(),
    })
}

/// Generate DUT artifacts (`fw_cfg_data.rs` + enabled `fw_us_*` names) from `config.toml`.
pub fn generate_fw_cfg(board_dir: &Path, out_dir: &Path) -> Result<FwCfgGen> {
    let cfg = config::generate(board_dir, out_dir)?;
    Ok(FwCfgGen {
        enabled_us_cfgs: cfg.enabled_us_cfgs,
        warnings: cfg.warnings,
    })
}

/// Runtime + fw_cfg into one `out_dir`.
pub fn generate(board_dir: &Path, out_dir: &Path) -> Result<GenResult> {
    layout::cross_check_firmware_config(board_dir, board_dir)?;
    let rt = generate_runtime(board_dir, out_dir)?;
    let cfg = generate_fw_cfg(board_dir, out_dir)?;
    let mut warnings = cfg.warnings;
    warnings.extend(rt.warnings);
    Ok(GenResult {
        enabled_us_cfgs: cfg.enabled_us_cfgs,
        custom_sections_path: rt.custom_sections_path,
        warnings,
    })
}
