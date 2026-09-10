//! xpost board codegen: reads `XPOST_BOARD` (dir with layout/mem/bpf.toml).
//!
//! Integrators (e.g. examples/firmware) set `XPOST_BOARD` via `.cargo/config.toml`.
//! Emits IRAM linker script path as `cargo:custom-sections=` for the final binary link.
//!
//! Feature `from-bpf-toml`: full bpf boot codegen. Without it, layout/mem/slots are
//! still generated when `XPOST_BOARD` is set, but bpf boot tables are empty stubs.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let from_bpf_toml = env::var_os("CARGO_FEATURE_FROM_BPF_TOML").is_some();
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FROM_BPF_TOML");

    let board = match env::var_os("XPOST_BOARD") {
        Some(p) => PathBuf::from(p),
        None => {
            if from_bpf_toml {
                panic!(
                    "from-bpf-toml requires XPOST_BOARD (board dir with bpf.toml); unset board is only allowed without that feature"
                );
            }
            let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
            write_stub_codegen(&out_dir);
            println!(
                "cargo:warning=XPOST_BOARD unset; using stub mem_map (set XPOST_BOARD to a board dir for real builds)"
            );
            println!(
                "cargo:custom-sections={}",
                out_dir.join("custom-sections.x").display()
            );
            emit_radio_wrap();
            return;
        }
    };

    println!("cargo:rerun-if-env-changed=XPOST_BOARD");
    println!(
        "cargo:rerun-if-changed={}",
        board.join("layout.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        board.join("mem.toml").display()
    );
    if from_bpf_toml {
        let bpf = board.join("bpf.toml");
        println!("cargo:rerun-if-changed={}", bpf.display());
        if !bpf.is_file() {
            panic!(
                "from-bpf-toml requires {}: file not found",
                bpf.display()
            );
        }
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let result = if from_bpf_toml {
        xpost_build::generate_runtime(&board, &out_dir)
            .unwrap_or_else(|e| panic!("xpost board codegen ({}): {e:#}", board.display()))
    } else {
        let layout = xpost_build::generate_layout_runtime(&board, &out_dir)
            .unwrap_or_else(|e| panic!("xpost layout codegen ({}): {e:#}", board.display()));
        write_empty_bpf_boot_stubs(&out_dir);
        layout
    };

    for w in &result.warnings {
        println!("cargo:warning={w}");
    }

    println!(
        "cargo:custom-sections={}",
        result.custom_sections_path.display()
    );
    emit_radio_wrap();
}

fn emit_radio_wrap() {
    // Capture STA/AP RX registration so XDP runs before esp-radio's private queue.
    if env::var_os("CARGO_FEATURE_ESP32C3_RADIO").is_some() {
        println!("cargo:rustc-link-arg=--wrap=esp_wifi_internal_reg_rxcb");
    }
}

fn write_empty_bpf_boot_stubs(out_dir: &PathBuf) {
    fs::write(
        out_dir.join("bpf_maps.rs"),
        r#"// empty bpf boot stubs (from-bpf-toml feature off)
pub const FIRMWARE_MAP_COUNT: usize = 0;
pub const FIRMWARE_HAS_ARRAY: bool = false;
pub const FIRMWARE_HAS_HASH: bool = false;
pub const FIRMWARE_HAS_RINGBUF: bool = false;
pub const SELFTEST_MAP_ID: crate::map::MapIdx = 0;
pub const SELFTEST_MAP_TYPE: &str = "stub";
pub const SELFTEST_KEY_SIZE: usize = 4;
pub const SELFTEST_VALUE_SIZE: usize = 8;
"#,
    )
    .unwrap();
    fs::write(
        out_dir.join("xdp_bpf.rs"),
        "pub(crate) static BOOT_IMAGES: &[BpfTomlImage] = &[];\n",
    )
    .unwrap();
    fs::write(
        out_dir.join("aot_map_bind.rs"),
        r#"pub fn bind_aot_map_addrs() {}
pub const AOT_MAP_BIND_COUNT: usize = 0;
pub const AOT_MAP_BINDS: &[(&str, usize)] = &[];
pub const AOT_MAP_USER_NAMES: &[&str] = &[];
"#,
    )
    .unwrap();
}

fn write_stub_codegen(out_dir: &PathBuf) {
    fs::write(
        out_dir.join("mem_map.rs"),
        r#"// stub — set XPOST_BOARD for real layout
pub const IRAM_BASE: usize = 0;
pub const ARENA_END: usize = 0;
pub const HELPER_DISPATCHER_BASE: usize = 0;
pub const HELPER_WINDOW_LEN: usize = 0;
pub const HOT_SLOT_COUNT: usize = 2;
pub const HOT_SLOT_BASES: [usize; 2] = [0, 0];
pub const HOT_SLOT_LENS: [usize; 2] = [0, 0];
pub const HOT_SLOT_NAMES: [&str; 2] = ["a", "b"];
pub const HOT_SLOT_MOUNTS: [&str; 2] = ["rx", "call"];
pub const MOUNT_RX_SLOT: Option<usize> = Some(0);
pub const HOT_SLOT_LEN_MAX: usize = 0;
pub const HOT_A_BASE: usize = 0;
pub const HOT_A_LEN: usize = 0;
pub const HOT_REGION_BASE: usize = 0;
pub const HOT_REGION_LEN: usize = 0;
pub const HOT_B_BASE: usize = 0;
pub const HOT_B_LEN: usize = 0;
pub fn assert_linked_addresses(helper_addr: usize, slot_addrs: &[usize]) {
    let _ = (helper_addr, slot_addrs);
}
pub const unsafe fn firmware_xdp_slots() -> [crate::xdp::XdpSlot; HOT_SLOT_COUNT] {
    [
        unsafe { crate::xdp::XdpSlot::from_raw_parts(HOT_SLOT_BASES[0], HOT_SLOT_LENS[0], true) },
        unsafe { crate::xdp::XdpSlot::from_raw_parts(HOT_SLOT_BASES[1], HOT_SLOT_LENS[1], false) },
    ]
}
"#,
    )
    .unwrap();
    fs::write(
        out_dir.join("hot_slots.rs"),
        r#"// stub hot slots
pub fn slot_buf(_i: usize) -> (*mut u8, usize) {
    (core::ptr::null_mut(), 0)
}
pub fn slot_link_addrs() -> [usize; 2] {
    [0, 0]
}
"#,
    )
    .unwrap();
    write_empty_bpf_boot_stubs(out_dir);
    fs::write(
        out_dir.join("xdp_bpf.rs"),
        "pub(crate) static BOOT_IMAGES: &[BpfTomlImage] = &[];\n",
    )
    .unwrap();
    fs::write(
        out_dir.join("aot_map_bind.rs"),
        r#"pub fn bind_aot_map_addrs() {}
pub const AOT_MAP_BIND_COUNT: usize = 0;
pub const AOT_MAP_BINDS: &[(&str, usize)] = &[];
pub const AOT_MAP_USER_NAMES: &[&str] = &[];
"#,
    )
    .unwrap();
    fs::write(out_dir.join("custom-sections.x"), "/* stub */\n").unwrap();
}
