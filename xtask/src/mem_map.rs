//! Parse generated `mem.toml` → `mem_map.rs` + `hot_slots.rs` + `custom-sections.x`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::layout::{MOUNT_CALL, MountKind};
use crate::util::{escape_rust_str, parse_addr};

pub struct MemMapGen {
    pub custom_sections_path: PathBuf,
    #[allow(dead_code)]
    pub slot_count: usize,
}

#[derive(Clone)]
struct Region {
    name: String,
    base: usize,
    length: usize,
    mount: MountKind,
}

pub fn generate(manifest_dir: &Path, out_dir: &Path) -> Result<MemMapGen> {
    let map_path = manifest_dir.join("mem.toml");
    let text = fs::read_to_string(&map_path)
        .with_context(|| format!("failed to read {}", map_path.display()))?;
    let root: toml::Table = text.parse().with_context(|| "failed to parse mem.toml")?;

    let arena = root.get("arena").and_then(|v| v.as_table());
    let helper_t = root
        .get("helper")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("mem.toml: missing [helper]"))?;

    let helper_base = parse_addr(helper_t, "base", "helper");
    let helper_len = parse_addr(helper_t, "length", "helper");

    let hot_regions = parse_hot_regions(&root)?;
    if hot_regions.is_empty() {
        bail!("mem.toml: need at least one [[hot_regions]]");
    }
    if hot_regions.len() > crate::MAX_SLOT_COUNT {
        bail!(
            "mem.toml: {} hot_regions exceeds MAX_SLOT_COUNT {}",
            hot_regions.len(),
            crate::MAX_SLOT_COUNT
        );
    }

    let iram_base = arena
        .map(|t| parse_addr(t, "iram_base", "arena"))
        .unwrap_or(helper_base);
    let arena_end = arena
        .map(|t| parse_addr(t, "arena_end", "arena"))
        .unwrap_or_else(|| {
            let last = hot_regions.last().unwrap();
            last.base + last.length
        });

    validate_layout(iram_base, arena_end, helper_base, helper_len, &hot_regions)?;

    write_mem_map_rs(
        out_dir,
        helper_base,
        helper_len,
        &hot_regions,
        iram_base,
        arena_end,
    )?;
    write_hot_slots_rs(out_dir, &hot_regions)?;
    let sections_path = write_custom_sections(out_dir, helper_base, helper_len, &hot_regions)?;

    Ok(MemMapGen {
        custom_sections_path: sections_path,
        slot_count: hot_regions.len(),
    })
}

fn parse_hot_regions(root: &toml::Table) -> Result<Vec<Region>> {
    if let Some(arr) = root.get("hot_regions").and_then(|v| v.as_array()) {
        let mut regions = Vec::with_capacity(arr.len());
        for (i, entry) in arr.iter().enumerate() {
            let table = entry
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("mem.toml: hot_regions[{i}] must be a table"))?;
            let base = parse_addr(table, "base", "hot_regions");
            let length = parse_addr(table, "length", "hot_regions");
            let name = table
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("slot{i}"));
            let mount_s = table
                .get("mount")
                .and_then(|v| v.as_str())
                .unwrap_or(MOUNT_CALL);
            let mount = MountKind::parse(mount_s)?;
            regions.push(Region {
                name,
                base,
                length,
                mount,
            });
        }
        return Ok(regions);
    }

    let mut regions = Vec::new();
    if let Some(a) = root.get("hot_region_a").and_then(|v| v.as_table()) {
        regions.push(Region {
            name: "a".into(),
            base: parse_addr(a, "base", "hot_region_a"),
            length: parse_addr(a, "length", "hot_region_a"),
            mount: MountKind::Call,
        });
    }
    if let Some(b) = root.get("hot_region_b").and_then(|v| v.as_table()) {
        regions.push(Region {
            name: "b".into(),
            base: parse_addr(b, "base", "hot_region_b"),
            length: parse_addr(b, "length", "hot_region_b"),
            mount: MountKind::Call,
        });
    }
    Ok(regions)
}

fn validate_layout(
    iram_base: usize,
    arena_end: usize,
    helper_base: usize,
    helper_len: usize,
    hot: &[Region],
) -> Result<()> {
    if helper_base != iram_base {
        bail!(
            "helper.base {:#x} must equal arena.iram_base {:#x}",
            helper_base,
            iram_base
        );
    }
    if !(helper_base.is_multiple_of(4) && helper_len > 0 && helper_len.is_multiple_of(4)) {
        bail!("helper base/length must be >0 and 4-byte aligned");
    }
    let mut expect = helper_base + helper_len;
    for (i, r) in hot.iter().enumerate() {
        if !(r.base.is_multiple_of(4) && r.length > 0 && r.length.is_multiple_of(4)) {
            bail!("hot_regions[{i}] base/length must be >0 and 4-byte aligned");
        }
        if r.base != expect {
            bail!(
                "hot_regions[{i}] base {:#x} must abut previous end {expect:#x}",
                r.base
            );
        }
        expect = r.base + r.length;
    }
    if expect != arena_end {
        bail!("layout end {expect:#x} != arena_end {arena_end:#x}");
    }
    let used = helper_len + hot.iter().map(|r| r.length).sum::<usize>();
    if used != arena_end - iram_base {
        bail!(
            "helper+slots {used:#x} != arena len {:#x}",
            arena_end - iram_base
        );
    }
    Ok(())
}

fn write_mem_map_rs(
    out_dir: &Path,
    helper_base: usize,
    helper_len: usize,
    hot: &[Region],
    iram_base: usize,
    arena_end: usize,
) -> Result<()> {
    let n = hot.len();
    let mut bases = String::new();
    let mut lens = String::new();
    let mut names = String::new();
    let mut mounts = String::new();
    for (i, r) in hot.iter().enumerate() {
        if i > 0 {
            bases.push_str(", ");
            lens.push_str(", ");
            names.push_str(", ");
            mounts.push_str(", ");
        }
        write!(bases, "{:#x}", r.base).unwrap();
        write!(lens, "{:#x}", r.length).unwrap();
        write!(names, "\"{}\"", escape_rust_str(&r.name)).unwrap();
        write!(mounts, "\"{}\"", r.mount.as_str()).unwrap();
    }

    let mut rx_slot = String::from("None");
    let mut tx_slot = String::from("None");
    for (i, r) in hot.iter().enumerate() {
        match r.mount {
            MountKind::Rx => rx_slot = format!("Some({i})"),
            MountKind::Tx => tx_slot = format!("Some({i})"),
            MountKind::Call => {}
        }
    }

    let mut rs = String::new();
    writeln!(rs, "// @generated by xtask from mem.toml — do not edit.\n").unwrap();
    writeln!(
        rs,
        "/// Patch arena IRAM base (helper window start).\n\
         pub const IRAM_BASE: usize = {iram_base:#x};\n\
         /// Exclusive end of patch arena (typically .trap).\n\
         pub const ARENA_END: usize = {arena_end:#x};\n\
         /// Absolute IRAM address of `helper_dispatcher`.\n\
         pub const HELPER_DISPATCHER_BASE: usize = {helper_base:#x};\n\
         /// Reserved IRAM window size for the helper trampoline.\n\
         pub const HELPER_WINDOW_LEN: usize = {helper_len:#x};\n\
         /// Number of IRAM hot-load slots.\n\
         pub const HOT_SLOT_COUNT: usize = {n};\n\
         /// Absolute IRAM base for each hot slot.\n\
         pub const HOT_SLOT_BASES: [usize; {n}] = [{bases}];\n\
         /// Capacity in bytes for each hot slot.\n\
         pub const HOT_SLOT_LENS: [usize; {n}] = [{lens}];\n\
         /// Logical slot names from config.toml `[slots]`.\n\
         pub const HOT_SLOT_NAMES: [&str; {n}] = [{names}];\n\
         /// Boot-time mount role per slot (`call` / `rx` / `tx`).\n\
         pub const HOT_SLOT_MOUNTS: [&str; {n}] = [{mounts}];\n\
         /// Index of the unique `rx` mount, if any.\n\
         pub const MOUNT_RX_SLOT: Option<usize> = {rx_slot};\n\
         /// Index of the unique `tx` mount, if any.\n\
         pub const MOUNT_TX_SLOT: Option<usize> = {tx_slot};\n\
         /// Largest hot-slot capacity (DRAM staging sized to this).\n\
         pub const HOT_SLOT_LEN_MAX: usize = {len_max};",
        len_max = hot.iter().map(|r| r.length).max().unwrap_or(0),
    )
    .unwrap();

    if n >= 1 {
        writeln!(
            rs,
            "\n/// Slot 0 base.\n\
             pub const HOT_A_BASE: usize = HOT_SLOT_BASES[0];\n\
             /// Slot 0 length.\n\
             pub const HOT_A_LEN: usize = HOT_SLOT_LENS[0];\n\
             /// Alias: slot 0 base.\n\
             pub const HOT_REGION_BASE: usize = HOT_A_BASE;\n\
             /// Alias: slot 0 length.\n\
             pub const HOT_REGION_LEN: usize = HOT_A_LEN;"
        )
        .unwrap();
    }
    if n >= 2 {
        writeln!(
            rs,
            "\n/// Slot 1 base.\n\
             pub const HOT_B_BASE: usize = HOT_SLOT_BASES[1];\n\
             /// Slot 1 length.\n\
             pub const HOT_B_LEN: usize = HOT_SLOT_LENS[1];"
        )
        .unwrap();
    }

    writeln!(
        rs,
        "\n/// Panic if linker placed symbols off the mem_map contract.\n\
         pub fn assert_linked_addresses(helper_addr: usize, slot_addrs: &[usize]) {{\n\
             assert_eq!(\n\
                 helper_addr, HELPER_DISPATCHER_BASE,\n\
                 \"helper_dispatcher linked at {{helper_addr:#x}}, expected {{HELPER_DISPATCHER_BASE:#x}}\"\n\
             );\n\
             assert_eq!(slot_addrs.len(), HOT_SLOT_COUNT, \"slot address count mismatch\");\n\
             for (i, &addr) in slot_addrs.iter().enumerate() {{\n\
                 let expected = HOT_SLOT_BASES[i];\n\
                 assert_eq!(\n\
                     addr, expected,\n\
                     \"HOT_SLOT[{{i}}] linked at {{addr:#x}}, expected {{expected:#x}}\"\n\
                 );\n\
             }}\n\
         }}\n"
    )
    .unwrap();

    fs::write(out_dir.join("mem_map.rs"), rs).context("write mem_map.rs")?;
    Ok(())
}

fn write_hot_slots_rs(out_dir: &Path, hot: &[Region]) -> Result<()> {
    let n = hot.len();
    let mut rs = String::new();
    writeln!(
        rs,
        "// @generated by xtask from mem.toml — do not edit.\n\
         // Included from patch::part for IRAM hot-slot buffers.\n"
    )
    .unwrap();

    for i in 0..n {
        writeln!(
            rs,
            "#[unsafe(link_section = \".iram1.hotload_{i}\")]\n\
             #[unsafe(no_mangle)]\n\
             static mut HOT_SLOT_{i}: [u8; super::mem_map::HOT_SLOT_LENS[{i}]] =\n\
                 [0u8; super::mem_map::HOT_SLOT_LENS[{i}]];\n"
        )
        .unwrap();
    }

    writeln!(
        rs,
        "/// IRAM buffer pointer and capacity for hot slot `i`.\n\
         pub fn slot_buf(i: usize) -> (*mut u8, usize) {{\n\
             match i {{"
    )
    .unwrap();
    for i in 0..n {
        writeln!(
            rs,
            "                {i} => (\n\
                     core::ptr::addr_of_mut!(HOT_SLOT_{i}) as *mut u8,\n\
                     super::mem_map::HOT_SLOT_LENS[{i}],\n\
                 ),"
        )
        .unwrap();
    }
    writeln!(
        rs,
        "                _ => core::panic!(\"hot slot index out of range\"),\n\
             }}\n\
         }}\n\
         \n\
         /// Absolute addresses of all hot-slot buffers (for [`assert_linked_addresses`]).\n\
         pub fn slot_link_addrs() -> [usize; {n}] {{\n\
             ["
    )
    .unwrap();
    for i in 0..n {
        if i > 0 {
            write!(rs, ", ").unwrap();
        }
        write!(rs, "core::ptr::addr_of!(HOT_SLOT_{i}) as usize").unwrap();
    }
    writeln!(
        rs,
        "]\n\
         }}\n"
    )
    .unwrap();

    fs::write(out_dir.join("hot_slots.rs"), rs).context("write hot_slots.rs")?;
    Ok(())
}

fn write_custom_sections(
    out_dir: &Path,
    helper_base: usize,
    helper_len: usize,
    hot: &[Region],
) -> Result<PathBuf> {
    let helper_end = helper_base + helper_len;
    let mut sections = String::new();
    writeln!(
        sections,
        "/* @generated by xtask from mem.toml — do not edit. */\n\
         SECTIONS\n\
         {{\n\
             .iram1.helper_dispatcher {helper_base:#x} :\n\
             {{\n\
                 . = ALIGN(4);\n\
                 KEEP(*(.iram1.helper_dispatcher))\n\
                 . = {helper_end:#x};\n\
             }} > IRAM\n",
    )
    .unwrap();

    for (i, r) in hot.iter().enumerate() {
        let end = r.base + r.length;
        writeln!(
            sections,
            "             .iram1.hotload_{i} {base:#x} :\n\
             {{\n\
                 . = ALIGN(4);\n\
                 KEEP(*(.iram1.hotload_{i}))\n\
                 . = {end:#x};\n\
             }} > IRAM\n",
            base = r.base,
        )
        .unwrap();
    }
    writeln!(sections, "         }}\n").unwrap();

    writeln!(
        sections,
        "         SECTIONS\n\
         {{\n\
             .rwdata_dummy_bs (NOLOAD) : ALIGN(4)\n\
             {{\n\
                 /* Skip DRAM aliases of helper + hot slots. */\n\
                 . = . + SIZEOF(.iram1.helper_dispatcher);"
    )
    .unwrap();
    for i in 0..hot.len() {
        writeln!(
            sections,
            "                 . = . + SIZEOF(.iram1.hotload_{i});"
        )
        .unwrap();
    }
    writeln!(
        sections,
        "             }} > DRAM\n\
         }}\n\
         INSERT BEFORE .data;\n"
    )
    .unwrap();

    let sections_path = out_dir.join("custom-sections.x");
    fs::write(&sections_path, sections).context("write custom-sections.x")?;
    Ok(sections_path)
}
