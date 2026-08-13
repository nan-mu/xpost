//! Firmware codegen library — invoked from `build.rs` and the `xtask` CLI.

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

/// CLI: validate `config.toml` layout and write `mem.toml`.
pub fn doctor(manifest_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(manifest_dir)
}

/// Deprecated alias for [`doctor`].
pub fn layout_apply(manifest_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(manifest_dir)
}

/// CLI: print IBUS/DBUS dual-map SRAM diagram from `mem.toml`.
pub fn map_print(manifest_dir: &Path) -> Result<()> {
    map_view::run_map(manifest_dir)
}

/// Must match `HEAP_SIZE` in `src/patch/map/pool.rs`.
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
    "xdp_rx",
    "hw_selftest",
    "map_crud",
    "cold_boot",
    "tx_hijack",
];

/// Known `[[tests]].name` values (router-probe registry alignment).
pub const KNOWN_TESTS: &[&str] = &[
    "firewall_echo",
    "xdp_forward_echo",
    "hot_load_ctrl",
    "fw_hash_test",
    "tail_call_abc",
    "tx_xdp_encap",
    "hw_selftest",
];

pub struct GenResult {
    pub enabled_us_cfgs: Vec<String>,
    pub custom_sections_path: PathBuf,
    pub warnings: Vec<String>,
}

/// Generate firmware build artifacts under `out_dir`.
///
/// Does **not** rewrite `mem.toml`. Requires it to match `config.toml` layout
/// (run `cargo xtask doctor` after editing `[arena]`/`[helper]`/`[slots]`).
pub fn generate(manifest_dir: &Path, out_dir: &Path) -> Result<GenResult> {
    layout::assert_mem_matches_config(manifest_dir)?;
    if std::env::var_os("XTASK_PRINT_MAP").is_some() {
        let _ = map_view::run_map(manifest_dir);
    }
    let cfg = config::generate(manifest_dir, out_dir)?;
    let mem = mem_map::generate(manifest_dir, out_dir)?;
    bpf_maps::generate(manifest_dir, out_dir)?;
    let aot = aot_bind::generate(manifest_dir, out_dir)?;

    let mut warnings = cfg.warnings;
    warnings.extend(aot.warnings);

    Ok(GenResult {
        enabled_us_cfgs: cfg.enabled_us_cfgs,
        custom_sections_path: mem.custom_sections_path,
        warnings,
    })
}
