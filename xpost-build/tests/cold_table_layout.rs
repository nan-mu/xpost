use std::fs;
use std::path::PathBuf;

use xpost_build::doctor;

fn board_with_cold() -> PathBuf {
    let dir = tempfile_dir();
    fs::write(
        dir.join("layout.toml"),
        r#"
[arena]
iram_base = 0x40380000
arena_end = 0x40382000
[helper]
length = 0x200
[slots]
a = { length = 0xE00, mount = "rx" }
b = { length = 0x1000, mount = "call" }
[cold_table]
base = 0x310000
length = 0x10000
image_capacity = 0x1000
[maps]
names = ["rxcnt", "firewall_array"]
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
fn doctor_exports_cold_table_and_maps() {
    let board = board_with_cold();
    let report = doctor(&board).unwrap();
    assert!(report.layout.cold_table.is_some());
    assert_eq!(
        report.layout.maps_names,
        vec!["rxcnt".to_string(), "firewall_array".to_string()]
    );
    let contract = board.join("target/layout-export/layout.toml");
    let text = fs::read_to_string(&contract).unwrap();
    assert!(text.contains("[cold_table]"));
    assert!(text.contains("base = 0x310000"));
    assert!(text.contains("\"rxcnt\""));
    let wb = board.join("target/layout-export/write-bin-addrs.toml");
    let wb_text = fs::read_to_string(&wb).unwrap();
    assert!(wb_text.contains("image0_abs = 0x310000"));
    assert!(wb_text.contains("image1_abs = 0x311000"));
    assert!(wb_text.contains("helper_dispatcher = 0x40380000"));
    assert_eq!(
        report.layout.cold_table.as_ref().unwrap().image_abs(1),
        Some(0x311000)
    );
    let _ = fs::remove_dir_all(&board);
}
