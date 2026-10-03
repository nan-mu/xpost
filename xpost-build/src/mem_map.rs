//! Emit `mem_map.rs` + `hot_slots.rs` + `custom-sections.x` from in-memory [`Layout`].

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::layout::{ColdTable, Layout, MountKind};
use crate::util::escape_rust_str;

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

/// Generate IRAM codegen artifacts from a computed [`Layout`].
pub fn generate(layout: &Layout, out_dir: &Path) -> Result<MemMapGen> {
    let hot_regions: Vec<Region> = layout
        .hot_regions
        .iter()
        .map(|r| Region {
            name: r.name.clone(),
            base: r.base,
            length: r.length,
            mount: r.mount,
        })
        .collect();

    if hot_regions.is_empty() {
        bail!("layout: need at least one hot slot");
    }
    if hot_regions.len() > crate::MAX_SLOT_COUNT {
        bail!(
            "layout: {} hot slots exceeds MAX_SLOT_COUNT {}",
            hot_regions.len(),
            crate::MAX_SLOT_COUNT
        );
    }

    let helper_base = layout.helper.base;
    let helper_len = layout.helper.length;
    let iram_base = layout.iram_base;
    let arena_end = layout.arena_end;

    validate_layout(iram_base, arena_end, helper_base, helper_len, &hot_regions)?;

    write_mem_map_rs(MemMapWrite {
        out_dir,
        helper_base,
        helper_len,
        hot: &hot_regions,
        iram_base,
        arena_end,
        cold: layout.cold_table.as_ref(),
        maps_names: &layout.maps_names,
    })?;
    write_hot_slots_rs(out_dir, &hot_regions)?;
    let sections_path = write_custom_sections(out_dir, helper_base, helper_len, &hot_regions)?;

    Ok(MemMapGen {
        custom_sections_path: sections_path,
        slot_count: hot_regions.len(),
    })
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
    // Default: slots are firmware-owned (base == 0). Only helper fills the IRAM patch arena.
    let pinned = hot.iter().any(|r| r.base != 0);
    if !pinned {
        for (i, r) in hot.iter().enumerate() {
            if !(r.length > 0 && r.length.is_multiple_of(4)) {
                bail!("slots[{i}] length must be >0 and 4-byte aligned");
            }
        }
        if helper_base + helper_len != arena_end {
            bail!(
                "helper-only arena end {:#x} != arena_end {arena_end:#x}",
                helper_base + helper_len
            );
        }
        return Ok(());
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

struct MemMapWrite<'a> {
    out_dir: &'a Path,
    helper_base: usize,
    helper_len: usize,
    hot: &'a [Region],
    iram_base: usize,
    arena_end: usize,
    cold: Option<&'a ColdTable>,
    maps_names: &'a [String],
}

fn write_mem_map_rs(
    MemMapWrite {
        out_dir,
        helper_base,
        helper_len,
        hot,
        iram_base,
        arena_end,
        cold,
        maps_names,
    }: MemMapWrite<'_>,
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
    for (i, r) in hot.iter().enumerate() {
        match r.mount {
            MountKind::Rx => rx_slot = format!("Some({i})"),
            MountKind::Call => {}
        }
    }

    let (cold_base, cold_len, cold_cap, cold_count, cold_present) = match cold {
        Some(c) => (
            c.base,
            c.length,
            c.image_capacity,
            c.image_count(),
            true,
        ),
        None => (0, 0, 0, 0, false),
    };

    let mut map_names_rs = String::new();
    for (i, name) in maps_names.iter().enumerate() {
        if i > 0 {
            map_names_rs.push_str(", ");
        }
        write!(map_names_rs, "\"{}\"", escape_rust_str(name)).unwrap();
    }

    let mut rs = String::new();
    writeln!(rs, "// @generated by xtask from layout.toml — do not edit.\n").unwrap();
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
         /// Logical slot names from layout.toml `[slots]`.\n\
         pub const HOT_SLOT_NAMES: [&str; {n}] = [{names}];\n\
         /// Boot-time mount role per slot (`call` / `rx`).\n\
         pub const HOT_SLOT_MOUNTS: [&str; {n}] = [{mounts}];\n\
         /// Index of the unique `rx` mount, if any.\n\
         pub const MOUNT_RX_SLOT: Option<usize> = {rx_slot};\n\
         /// Largest hot-slot capacity (DRAM staging sized to this).\n\
         pub const HOT_SLOT_LEN_MAX: usize = {len_max};\n\
         /// Whether intent declared `[cold_table]` (Flash write-bin path).\n\
         pub const COLD_TABLE_PRESENT: bool = {cold_present};\n\
         /// Absolute flash address of the packed Compact ELF region.\n\
         pub const COLD_TABLE_FLASH_BASE: usize = {cold_base:#x};\n\
         /// Cold-table total length in bytes.\n\
         pub const COLD_TABLE_FLASH_LEN: usize = {cold_len:#x};\n\
         /// Max bytes of one Compact ELF container (not a write stride).\n\
         pub const COLD_TABLE_IMAGE_CAPACITY: usize = {cold_cap:#x};\n\
         /// `FLASH_LEN / IMAGE_CAPACITY` (upper bound on packed images).\n\
         pub const COLD_TABLE_IMAGE_COUNT: usize = {cold_count};\n\
         /// Map names from intent `[maps].names` (may be empty).\n\
         pub const LAYOUT_MAP_NAMES: &[&str] = &[{map_names_rs}];\n\
         /// Absolute flash address of cold-table cell `index`, if in range.\n\
         pub const fn cold_table_image_abs(index: usize) -> Option<usize> {{\n\
             if !COLD_TABLE_PRESENT || COLD_TABLE_IMAGE_CAPACITY == 0 {{\n\
                 return None;\n\
             }}\n\
             if index >= COLD_TABLE_IMAGE_COUNT {{\n\
                 return None;\n\
             }}\n\
             Some(COLD_TABLE_FLASH_BASE + index * COLD_TABLE_IMAGE_CAPACITY)\n\
         }}",
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
         }}\n\
         \n\
         /// Build immutable [`crate::xdp::XdpSlot`] descriptors for `XdpRt::init`.\n\
         ///\n\
         /// # Safety\n\
         /// Caller must only use these with the matching IRAM regions for the\n\
         /// firmware lifetime; primary is the unique `rx` mount (else slot 0).\n\
         pub const unsafe fn firmware_xdp_slots() -> [::xpost::xdp::XdpSlot; HOT_SLOT_COUNT] {{\n\
             ["
    )
    .unwrap();
    for i in 0..n {
        let primary = match &rx_slot {
            s if s == &format!("Some({i})") => "true",
            _ if i == 0 && rx_slot == "None" => "true",
            _ => "false",
        };
        writeln!(
            rs,
            "                 // SAFETY: bases/lens come from validated layout.toml intent.\n\
                     unsafe {{ ::xpost::xdp::XdpSlot::from_raw_parts(HOT_SLOT_BASES[{i}], HOT_SLOT_LENS[{i}], {primary}) }},"
        )
        .unwrap();
    }
    writeln!(
        rs,
        "             ]\n\
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
        "// @generated by xtask from layout.toml — do not edit.\n\
         // Included from patch::part for IRAM hot-slot buffers.\n"
    )
    .unwrap();

    for i in 0..n {
        writeln!(
            rs,
            "#[unsafe(link_section = \".xdp.hotload_{i}\")]\n\
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
    let mut sections = String::new();
    writeln!(
        sections,
        "/* @generated by xtask from layout.toml — do not edit. */\n\
         SECTIONS\n\
         {{\n\
             .xdp.helper_dispatcher {helper_base:#x} : ALIGN(4)\n\
             {{\n\
                 KEEP(*(.xdp.helper_dispatcher .xdp.helper_dispatcher.*))\n\
                 . = {helper_end:#x};\n\
             }} > IRAM\n",
        helper_end = helper_base + helper_len,
    )
    .unwrap();

    for (i, r) in hot.iter().enumerate() {
        writeln!(
            sections,
            "             .xdp.hotload_{i} {base:#x} : ALIGN(4)\n\
             {{\n\
                 KEEP(*(.xdp.hotload_{i} .xdp.hotload_{i}.*))\n\
                 . = {end:#x};\n\
             }} > IRAM\n",
            base = r.base,
            end = r.base + r.length,
        )
        .unwrap();
    }
    writeln!(
        sections,
        "         }}\n\
         INSERT BEFORE .trap;\n"
    )
    .unwrap();

    let arena_len = helper_len + hot.iter().map(|r| r.length).sum::<usize>();
    writeln!(
        sections,
        "         SECTIONS\n\
         {{\n\
             .rwdata_dummy_bs (NOLOAD) : ALIGN(4)\n\
             {{\n\
                 /* Skip DRAM alias of the patch arena (IRAM head). */\n\
                 . = . + {arena_len:#x};\n\
             }} > DRAM\n\
         }}\n\
         INSERT BEFORE .rwdata_dummy;\n"
    )
    .unwrap();

    let sections_path = out_dir.join("custom-sections.x");
    fs::write(&sections_path, sections).context("write custom-sections.x")?;
    Ok(sections_path)
}
