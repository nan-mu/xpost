//! Board codegen library — layout/mem/bpf.toml → Rust + IRAM linker script.
//!
//! Board directory = directory containing `layout.toml`, `mem.toml`, `bpf.toml`
//! (and optionally `config.toml` for DUT cfgs). Used by example `build.rs` / xtask
//! and by the `xpost` crate `build.rs` via `XPOST_BOARD`.

mod aot_bind;
mod bpf_maps;
mod config;
mod layout;
mod map_view;
mod mem_map;
mod util;

use std::path::{Path, PathBuf};

use anyhow::Result;

pub use layout::{Layout, LayoutReport, MAX_SLOT_COUNT, MIN_SLOT_COUNT, MountKind};
pub use map_view::{BusType, RenderRow, Section};

/// Resolve board dir: `XPOST_BOARD` env, else `manifest_dir` (example package root).
pub fn board_dir(manifest_or_hint: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("XPOST_BOARD") {
        return PathBuf::from(p);
    }
    manifest_or_hint.to_path_buf()
}

/// Validate `layout.toml` and write `mem.toml` in `board_dir`.
pub fn doctor(board_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(board_dir)
}

/// Deprecated alias for [`doctor`].
pub fn layout_apply(board_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(board_dir)
}

/// Print IBUS/DBUS dual-map SRAM diagram from `mem.toml`.
pub fn map_print(board_dir: &Path) -> Result<()> {
    map_view::run_map(board_dir)
}

/// Must match `HEAP_SIZE` in `xpost` map pool.
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
/// `bpf_maps.rs`, `aot_map_bind.rs`) from tomls in `board_dir`.
pub fn generate_runtime(board_dir: &Path, out_dir: &Path) -> Result<RuntimeGen> {
    layout::assert_mem_matches_layout(board_dir)?;
    if std::env::var_os("XTASK_PRINT_MAP").is_some() {
        let _ = map_view::run_map(board_dir);
    }
    let mem = mem_map::generate(board_dir, out_dir)?;
    bpf_maps::generate(board_dir, out_dir)?;
    let aot = aot_bind::generate(board_dir, out_dir)?;
    Ok(RuntimeGen {
        custom_sections_path: mem.custom_sections_path,
        warnings: aot.warnings,
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
