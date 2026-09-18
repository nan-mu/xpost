//! xpost library build: stub mem_map for host / bare-lib checks.
//!
//! Board codegen lives in the firmware `build.rs` (`xpost-build` sync). This
//! `build.rs` does not read `XPOST_BOARD`.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    write_stub_codegen(&out_dir);
    println!(
        "cargo:custom-sections={}",
        out_dir.join("custom-sections.x").display()
    );
}

fn write_stub_codegen(out_dir: &PathBuf) {
    fs::write(
        out_dir.join("mem_map.rs"),
        r#"// stub — board codegen via firmware xpost-build sync
pub const IRAM_BASE: usize = 0;
pub const ARENA_END: usize = 0;
pub const HELPER_DISPATCHER_BASE: usize = 0;
pub const HELPER_WINDOW_LEN: usize = 0;
pub const SLOT_COUNT: usize = 2;
pub const SLOT_BASES: [usize; 2] = [0, 0];
pub const SLOT_LENS: [usize; 2] = [0, 0];
pub const SLOT_NAMES: [&str; 2] = ["a", "b"];
pub const SLOT_MOUNTS: [&str; 2] = ["rx", "call"];
pub const MOUNT_RX_SLOT: Option<usize> = Some(0);
pub const SLOT_LEN_MAX: usize = 0;
pub const SLOT_A_LEN: usize = 0;
pub const HOT_REGION_LEN: usize = 0;
pub const SLOT_B_LEN: usize = 0;
pub const COLD_TABLE_PRESENT: bool = false;
pub const COLD_TABLE_FLASH_BASE: usize = 0;
pub const COLD_TABLE_FLASH_LEN: usize = 0;
pub const COLD_TABLE_IMAGE_CAPACITY: usize = 0;
pub const COLD_TABLE_IMAGE_COUNT: usize = 0;
pub const LAYOUT_MAP_NAMES: &[&str] = &[];
pub const fn cold_table_image_abs(_index: usize) -> Option<usize> {
    None
}
pub fn assert_linked_addresses(helper_addr: usize, _slot_addrs: &[usize]) {
    let _ = helper_addr;
}
"#,
    )
    .unwrap();
    fs::write(
        out_dir.join("hot_slots.rs"),
        r#"// stub — firmware owns slot buffers
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
        out_dir.join("custom-sections.x"),
        "/* stub — firmware sync emits real linker fragment */\n",
    )
    .unwrap();
}
