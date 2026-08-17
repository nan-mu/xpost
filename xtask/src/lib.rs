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

/// Runtime crate directory (`xdp-mcu/`: layout.toml, mem.toml, bpf.toml).
pub fn runtime_dir(firmware_dir: &Path) -> PathBuf {
    firmware_dir.join("xdp-mcu")
}

/// CLI: validate `xdp-mcu/layout.toml` and write `xdp-mcu/mem.toml`.
pub fn doctor(firmware_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(firmware_dir)
}

/// Deprecated alias for [`doctor`].
pub fn layout_apply(firmware_dir: &Path) -> Result<LayoutReport> {
    layout::doctor(firmware_dir)
}

/// CLI: print IBUS/DBUS dual-map SRAM diagram from `xdp-mcu/mem.toml`.
pub fn map_print(firmware_dir: &Path) -> Result<()> {
    map_view::run_map(&runtime_dir(firmware_dir))
}

/// Must match `HEAP_SIZE` in `xdp-mcu/src/map/pool.rs`.
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

/// Generate xdp-mcu runtime artifacts (`mem_map.rs`, `hot_slots.rs`,
/// `custom-sections.x`, `bpf_maps.rs`, `aot_map_bind.rs`).
///
/// Does **not** rewrite `mem.toml`. Requires it to match `layout.toml`
/// (run `cargo xtask doctor` after editing `[arena]`/`[helper]`/`[slots]`).
pub fn generate_runtime(runtime_dir: &Path, out_dir: &Path) -> Result<RuntimeGen> {
    layout::assert_mem_matches_layout(runtime_dir)?;
    if std::env::var_os("XTASK_PRINT_MAP").is_some() {
        let _ = map_view::run_map(runtime_dir);
    }
    let mem = mem_map::generate(runtime_dir, out_dir)?;
    bpf_maps::generate(runtime_dir, out_dir)?;
    let aot = aot_bind::generate(runtime_dir, out_dir)?;
    Ok(RuntimeGen {
        custom_sections_path: mem.custom_sections_path,
        warnings: aot.warnings,
    })
}

/// Generate firmware DUT artifacts (`fw_cfg_data.rs` + enabled `fw_us_*` names).
pub fn generate_fw_cfg(firmware_dir: &Path, out_dir: &Path) -> Result<FwCfgGen> {
    let cfg = config::generate(firmware_dir, out_dir)?;
    Ok(FwCfgGen {
        enabled_us_cfgs: cfg.enabled_us_cfgs,
        warnings: cfg.warnings,
    })
}

/// CLI check/gen: runtime + fw_cfg into one `out_dir`.
pub fn generate(firmware_dir: &Path, out_dir: &Path) -> Result<GenResult> {
    let runtime = runtime_dir(firmware_dir);
    layout::cross_check_firmware_config(firmware_dir, &runtime)?;
    let rt = generate_runtime(&runtime, out_dir)?;
    let cfg = generate_fw_cfg(firmware_dir, out_dir)?;
    let mut warnings = cfg.warnings;
    warnings.extend(rt.warnings);
    Ok(GenResult {
        enabled_us_cfgs: cfg.enabled_us_cfgs,
        custom_sections_path: rt.custom_sections_path,
        warnings,
    })
}
