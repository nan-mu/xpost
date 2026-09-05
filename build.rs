//! xpost board codegen: reads `XPOST_BOARD` (dir with layout/mem/bpf.toml).
//!
//! Integrators (e.g. examples/firmware) set `XPOST_BOARD` via `.cargo/config.toml`.
//! Emits IRAM linker script path as `cargo:custom-sections=` for the final binary link.

use std::env;
use std::path::PathBuf;

fn main() {
    let board = match env::var_os("XPOST_BOARD") {
        Some(p) => PathBuf::from(p),
        None => {
            let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
            write_stub_codegen(&out_dir);
            println!(
                "cargo:warning=XPOST_BOARD unset; using stub mem_map (set XPOST_BOARD to a board dir for real builds)"
            );
            println!(
                "cargo:custom-sections={}",
                out_dir.join("custom-sections.x").display()
            );
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
    println!(
        "cargo:rerun-if-changed={}",
        board.join("bpf.toml").display()
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let result = xpost_build::generate_runtime(&board, &out_dir)
        .unwrap_or_else(|e| panic!("xpost board codegen ({}): {e:#}", board.display()));

    for w in &result.warnings {
        println!("cargo:warning={w}");
    }

    println!(
        "cargo:custom-sections={}",
        result.custom_sections_path.display()
    );
}

fn write_stub_codegen(out_dir: &PathBuf) {
    use std::fs;
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
    fs::write(
        out_dir.join("bpf_maps.rs"),
        r#"// stub — set XPOST_BOARD for real layout
pub async fn init_firmware_maps_via_builder(_rt: &mut crate::map::MapRt) -> Result<(), crate::map::Error> { Ok(()) }
"#,
    )
    .unwrap();
    fs::write(
        out_dir.join("xdp_bpf.rs"),
        "pub(crate) static BOOT_IMAGES: &[super::BpfTomlImage] = &[];\n",
    )
    .unwrap();
    fs::write(
        out_dir.join("aot_map_bind.rs"),
        r#"pub fn bind_aot_map_addrs() {}
pub const AOT_MAP_BIND_COUNT: usize = 0;
pub const AOT_MAP_USER_NAMES: &[&str] = &[];
"#,
    )
    .unwrap();
    fs::write(out_dir.join("custom-sections.x"), "/* stub */\n").unwrap();
}
