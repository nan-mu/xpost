//! Fake ELF + partitions.csv → embed `.xpost.mem` → section bytes match encode.

use std::fs;
use std::path::PathBuf;

use object::write::{Object as WObject, Symbol, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, Object, ObjectSection, SectionKind, SymbolFlags,
    SymbolKind, SymbolScope,
};
use xpost_build::{HELPER_SYMBOL, SLOT_SYMBOL, embed_layout, read_mem_section};
use xpost_elf::{MemMapV0, SECTION_NAME, encode};

fn tempfile_dir() -> PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!(
        "xpost-build-embed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn board_with_partition() -> PathBuf {
    let dir = tempfile_dir();
    fs::write(
        dir.join("partitions.csv"),
        r#"# Name, Type, SubType, Offset, Size, Flags
nvs, data, nvs, 0x9000, 0x6000,
phy_init, data, phy, 0xf000, 0x1000,
factory, app, factory, 0x10000, 0x300000,
xdp, 0x40, 0x00, 0x310000, 0x10000,
"#,
    )
    .unwrap();
    dir
}

fn write_fake_elf(path: &PathBuf) {
    let mut obj = WObject::new(
        BinaryFormat::Elf,
        Architecture::Riscv32,
        Endianness::Little,
    );
    let text = obj.add_section(Vec::new(), b".text".to_vec(), SectionKind::Text);
    obj.append_section_data(text, &[0u8; 0x1000], 4);

    obj.add_symbol(Symbol {
        name: HELPER_SYMBOL.as_bytes().to_vec(),
        value: 0x4038_0000,
        size: 64,
        kind: SymbolKind::Text,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(text),
        flags: SymbolFlags::None,
    });
    obj.add_symbol(Symbol {
        name: SLOT_SYMBOL.as_bytes().to_vec(),
        value: 0x4038_0200,
        size: 0xE00,
        kind: SymbolKind::Data,
        scope: SymbolScope::Linkage,
        weak: false,
        section: SymbolSection::Section(text),
        flags: SymbolFlags::None,
    });

    let bytes = obj.write().expect("write fake ELF");
    fs::write(path, bytes).unwrap();
}

#[test]
fn embed_writes_xpm1_section() {
    let board = board_with_partition();
    let crate_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
    let elf = board.join("firmware.elf");
    write_fake_elf(&elf);

    let map = embed_layout(&crate_manifest, &board, &elf).unwrap();
    assert_eq!(map.helper_addr, 0x4038_0000);
    assert_eq!(map.slot_addr, 0x4038_0200);
    assert_eq!(map.slot_len, 0xE00);
    assert_eq!(map.table_addr, 0x31_0000);
    assert_eq!(map.table_len, 0x1_0000);

    let expected = encode(&MemMapV0 {
        helper_addr: 0x4038_0000,
        slot_addr: 0x4038_0200,
        slot_len: 0xE00,
        table_addr: 0x31_0000,
        table_len: 0x1_0000,
    });
    assert_eq!(map.encode(), expected);

    let decoded = read_mem_section(&elf).unwrap();
    assert_eq!(decoded, map);

    // Raw section bytes must match encode (not just decode).
    let data = fs::read(&elf).unwrap();
    let file = object::File::parse(&*data).unwrap();
    let sec = file.section_by_name(SECTION_NAME).expect("section present");
    assert_eq!(sec.data().unwrap(), &expected[..]);

    // Idempotent overwrite of existing section.
    let map2 = embed_layout(&crate_manifest, &board, &elf).unwrap();
    assert_eq!(map2, map);

    let _ = fs::remove_dir_all(&board);
}
