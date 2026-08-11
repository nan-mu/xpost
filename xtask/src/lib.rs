//! Firmware codegen library — invoked from `build.rs` and the `xtask` CLI.

mod aot_bind;
mod bpf_maps;
mod config;
mod mem_map;
mod util;

use std::path::{Path, PathBuf};

use anyhow::Result;

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
];

/// Known `[[tests]].name` values (router-probe registry alignment).
pub const KNOWN_TESTS: &[&str] = &[
    "firewall_echo",
    "xdp_forward_echo",
    "hot_load_ctrl",
    "fw_hash_test",
    "hw_selftest",
];

pub struct GenResult {
    pub enabled_us_cfgs: Vec<String>,
    pub custom_sections_path: PathBuf,
    pub warnings: Vec<String>,
}

/// Generate all firmware build artifacts under `out_dir` from tomls in `manifest_dir`.
pub fn generate(manifest_dir: &Path, out_dir: &Path) -> Result<GenResult> {
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
