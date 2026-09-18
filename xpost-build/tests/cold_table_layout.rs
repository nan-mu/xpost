use std::fs;
use std::path::PathBuf;

use xpost_build::{SyncMode, SyncRequest, export};

fn board_with_partition() -> PathBuf {
    let dir = tempfile_dir();
    fs::write(
        dir.join("partitions.csv"),
        r#"# Name, Type, SubType, Offset, Size, Flags
nvs, data, nvs, 0x9000, 0x6000,
phy_init, data, phy, 0xf000, 0x1000,
factory, app, factory, 0x10000, 0x300000,
xdp, data, 0x40, 0x310000, 0x10000,
"#,
    )
    .unwrap();
    dir
}

fn tempfile_dir() -> PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!(
        "xpost-build-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn export_cold_table_from_partitions_csv() {
    let board = board_with_partition();
    let crate_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let out = board.join("target/layout-export");
    let contract = out.join("layout.toml");
    let req = SyncRequest {
        mode: SyncMode::Default,
        crate_manifest: &crate_manifest,
        board_dir: Some(&board),
        out_dir: &out,
        contract_out: Some(&contract),
        elf_path: None,
    };
    let report = export(&req).unwrap();
    assert!(report.layout.cold_table.is_some());
    let cold = report.layout.cold_table.as_ref().unwrap();
    assert_eq!(cold.base, 0x310000);
    assert_eq!(cold.length, 0x10000);
    let text = fs::read_to_string(&contract).unwrap();
    assert!(text.contains("[cold_table]"));
    assert!(text.contains("base = 0x310000"));
    let wb = out.join("write-bin-addrs.toml");
    let wb_text = fs::read_to_string(&wb).unwrap();
    assert!(wb_text.contains("0x310000"));
    assert!(wb_text.contains("helper_dispatcher = 0x40380000"));
    assert_eq!(cold.image_abs(1), Some(0x311000));
    // Slots are not absolute-pinned in default path.
    assert!(report.layout.hot_regions.iter().all(|r| r.base == 0));
    let _ = fs::remove_dir_all(&board);
}
