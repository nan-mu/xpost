//! Embed XPM1 mem-map into a linked firmware ELF as `.xpost.mem`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use object::{Object, ObjectSection, ObjectSymbol};

use xpost_elf::{MemMapV0, SECTION_NAME};

use crate::metadata::{load_from_cargo_toml, resolve_layout};

/// Firmware symbols required for the single-slot mem-map contract.
pub const HELPER_SYMBOL: &str = "helper_dispatcher";
pub const SLOT_SYMBOL: &str = "xdp_slot";

/// Collect addresses from ELF + `partitions.csv`, encode XPM1, write `.xpost.mem`.
pub fn embed_layout(
    crate_manifest: &Path,
    board_dir: &Path,
    elf_path: &Path,
) -> Result<MemMapV0> {
    let crate_meta = load_from_cargo_toml(crate_manifest)?
        .unwrap_or_else(crate::metadata::XpostMetadata::default_esp32c3);
    let (meta, _layout) = resolve_layout(&crate_meta, Some(board_dir))?;
    let Some(table) = meta.xdp_table.as_ref() else {
        bail!("embed_layout requires [package.metadata.xpost.xdp_table]");
    };
    if table.flash_len == 0 {
        bail!("embed_layout: cold table flash_len is 0 (check partitions.csv)");
    }

    let elf_data = fs::read(elf_path).with_context(|| format!("read {}", elf_path.display()))?;
    let (helper_addr, _) = symbol_addr_size(&elf_data, HELPER_SYMBOL)?;
    let (slot_addr, slot_len) = symbol_addr_size(&elf_data, SLOT_SYMBOL)?;
    if slot_len == 0 {
        bail!("ELF symbol `{SLOT_SYMBOL}` has size 0");
    }

    let map = MemMapV0 {
        helper_addr: u32::try_from(helper_addr)
            .map_err(|_| anyhow::anyhow!("helper_addr does not fit u32"))?,
        slot_addr: u32::try_from(slot_addr)
            .map_err(|_| anyhow::anyhow!("slot_addr does not fit u32"))?,
        slot_len: u32::try_from(slot_len).map_err(|_| anyhow::anyhow!("slot_len does not fit u32"))?,
        table_addr: u32::try_from(table.flash_base)
            .map_err(|_| anyhow::anyhow!("table_addr does not fit u32"))?,
        table_len: u32::try_from(table.flash_len)
            .map_err(|_| anyhow::anyhow!("table_len does not fit u32"))?,
    };
    let wire = map.encode();
    write_section(elf_path, SECTION_NAME, &wire)?;
    Ok(map)
}

fn symbol_addr_size(elf_data: &[u8], name: &str) -> Result<(u64, u64)> {
    let file = object::File::parse(elf_data).context("parse ELF")?;
    for sym in file.symbols() {
        if sym.name().ok() == Some(name) {
            return Ok((sym.address(), sym.size()));
        }
    }
    for sym in file.dynamic_symbols() {
        if sym.name().ok() == Some(name) {
            return Ok((sym.address(), sym.size()));
        }
    }
    bail!("ELF symbol `{name}` not found")
}

/// Read `.xpost.mem` payload from an ELF (for tests / host tools).
pub fn read_mem_section(elf_path: &Path) -> Result<MemMapV0> {
    let data = fs::read(elf_path).with_context(|| format!("read {}", elf_path.display()))?;
    let file = object::File::parse(&*data).context("parse ELF")?;
    let sec = file
        .section_by_name(SECTION_NAME)
        .ok_or_else(|| anyhow::anyhow!("ELF missing section `{SECTION_NAME}`"))?;
    let bytes = sec.data().context("section data")?;
    MemMapV0::decode(bytes).map_err(|e| anyhow::anyhow!("decode {SECTION_NAME}: {e:?}"))
}

/// Overwrite existing `.xpost.mem` in place, or append a new non-ALLOC PROGBITS section.
fn write_section(elf_path: &Path, name: &str, payload: &[u8]) -> Result<()> {
    let mut file = fs::read(elf_path).with_context(|| format!("read {}", elf_path.display()))?;
    {
        let obj = object::File::parse(&*file).context("parse ELF")?;
        if let Some(sec) = obj.section_by_name(name) {
            let Some((off, size)) = sec.file_range() else {
                bail!("section `{name}` has no file range");
            };
            let off = off as usize;
            let size = size as usize;
            if size < payload.len() {
                bail!(
                    "existing `{name}` size {size} < payload {}",
                    payload.len()
                );
            }
            file[off..off + payload.len()].copy_from_slice(payload);
            fs::write(elf_path, &file).with_context(|| format!("write {}", elf_path.display()))?;
            return Ok(());
        }
    }
    append_non_alloc_section(&mut file, name, payload)?;
    fs::write(elf_path, &file).with_context(|| format!("write {}", elf_path.display()))?;
    Ok(())
}

fn align_up(v: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    (v + a - 1) & !(a - 1)
}

fn append_non_alloc_section(file: &mut Vec<u8>, name: &str, payload: &[u8]) -> Result<()> {
    if file.len() < 16 || &file[0..4] != b"\x7fELF" {
        bail!("not an ELF file");
    }
    let class = file[4]; // 1=32, 2=64
    let data = file[5]; // 1=LE
    if data != 1 {
        bail!("only little-endian ELF supported for embed");
    }
    match class {
        1 => append_elf32(file, name, payload),
        2 => append_elf64(file, name, payload),
        _ => bail!("unsupported ELF class {class}"),
    }
}

fn u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}
fn u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
fn u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}
fn put_u16_le(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn put_u32_le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64_le(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn append_elf32(file: &mut Vec<u8>, name: &str, payload: &[u8]) -> Result<()> {
    const E_SHOFF: usize = 32;
    const E_SHENTSIZE: usize = 46;
    const E_SHNUM: usize = 48;
    const E_SHSTRNDX: usize = 50;
    const SHDR_SIZE: usize = 40;
    const SHT_PROGBITS: u32 = 1;
    const SHT_STRTAB: u32 = 3;

    if file.len() < 52 {
        bail!("ELF32 header truncated");
    }
    let shoff = u32_le(file, E_SHOFF) as usize;
    let shentsize = u16_le(file, E_SHENTSIZE) as usize;
    let shnum = u16_le(file, E_SHNUM) as usize;
    let shstrndx = u16_le(file, E_SHSTRNDX) as usize;
    if shentsize != SHDR_SIZE {
        bail!("unexpected ELF32 e_shentsize {shentsize}");
    }
    if shstrndx >= shnum {
        bail!("bad e_shstrndx");
    }
    let sht_end = shoff
        .checked_add(shnum.checked_mul(SHDR_SIZE).ok_or_else(|| anyhow::anyhow!("sht overflow"))?)
        .ok_or_else(|| anyhow::anyhow!("sht end overflow"))?;
    if sht_end > file.len() {
        bail!("section header table OOB");
    }

    let shstr_off = shoff + shstrndx * SHDR_SIZE;
    let old_str_off = u32_le(file, shstr_off + 16) as usize;
    let old_str_size = u32_le(file, shstr_off + 20) as usize;
    let sh_type = u32_le(file, shstr_off + 4);
    if sh_type != SHT_STRTAB {
        bail!("e_shstrndx is not SHT_STRTAB");
    }
    if old_str_off + old_str_size > file.len() {
        bail!(".shstrtab OOB");
    }
    let mut new_shstr = file[old_str_off..old_str_off + old_str_size].to_vec();
    let name_off = new_shstr.len() as u32;
    new_shstr.extend_from_slice(name.as_bytes());
    new_shstr.push(0);

    // Copy old headers before mutating the file buffer.
    let mut headers = file[shoff..sht_end].to_vec();

    let payload_off = align_up(file.len(), 4);
    file.resize(payload_off, 0);
    file.extend_from_slice(payload);

    let str_off = align_up(file.len(), 4);
    file.resize(str_off, 0);
    file.extend_from_slice(&new_shstr);

    // Update shstrtab header in the copy.
    put_u32_le(&mut headers, shstrndx * SHDR_SIZE + 16, str_off as u32);
    put_u32_le(&mut headers, shstrndx * SHDR_SIZE + 20, new_shstr.len() as u32);

    let mut new_shdr = [0u8; SHDR_SIZE];
    put_u32_le(&mut new_shdr, 0, name_off);
    put_u32_le(&mut new_shdr, 4, SHT_PROGBITS);
    put_u32_le(&mut new_shdr, 8, 0); // sh_flags: no ALLOC
    put_u32_le(&mut new_shdr, 12, 0); // sh_addr
    put_u32_le(&mut new_shdr, 16, payload_off as u32);
    put_u32_le(&mut new_shdr, 20, payload.len() as u32);
    put_u32_le(&mut new_shdr, 24, 0);
    put_u32_le(&mut new_shdr, 28, 0);
    put_u32_le(&mut new_shdr, 32, 4); // addralign
    put_u32_le(&mut new_shdr, 36, 0);
    headers.extend_from_slice(&new_shdr);

    let new_shoff = align_up(file.len(), 4);
    file.resize(new_shoff, 0);
    file.extend_from_slice(&headers);

    put_u32_le(file, E_SHOFF, new_shoff as u32);
    put_u16_le(file, E_SHNUM, (shnum + 1) as u16);
    Ok(())
}

fn append_elf64(file: &mut Vec<u8>, name: &str, payload: &[u8]) -> Result<()> {
    const E_SHOFF: usize = 40;
    const E_SHENTSIZE: usize = 58;
    const E_SHNUM: usize = 60;
    const E_SHSTRNDX: usize = 62;
    const SHDR_SIZE: usize = 64;
    const SHT_PROGBITS: u32 = 1;
    const SHT_STRTAB: u32 = 3;

    if file.len() < 64 {
        bail!("ELF64 header truncated");
    }
    let shoff = u64_le(file, E_SHOFF) as usize;
    let shentsize = u16_le(file, E_SHENTSIZE) as usize;
    let shnum = u16_le(file, E_SHNUM) as usize;
    let shstrndx = u16_le(file, E_SHSTRNDX) as usize;
    if shentsize != SHDR_SIZE {
        bail!("unexpected ELF64 e_shentsize {shentsize}");
    }
    if shstrndx >= shnum {
        bail!("bad e_shstrndx");
    }
    let sht_end = shoff
        .checked_add(shnum.checked_mul(SHDR_SIZE).ok_or_else(|| anyhow::anyhow!("sht overflow"))?)
        .ok_or_else(|| anyhow::anyhow!("sht end overflow"))?;
    if sht_end > file.len() {
        bail!("section header table OOB");
    }

    let shstr_off = shoff + shstrndx * SHDR_SIZE;
    let old_str_off = u64_le(file, shstr_off + 24) as usize;
    let old_str_size = u64_le(file, shstr_off + 32) as usize;
    let sh_type = u32_le(file, shstr_off + 4);
    if sh_type != SHT_STRTAB {
        bail!("e_shstrndx is not SHT_STRTAB");
    }
    if old_str_off + old_str_size > file.len() {
        bail!(".shstrtab OOB");
    }
    let mut new_shstr = file[old_str_off..old_str_off + old_str_size].to_vec();
    let name_off = new_shstr.len() as u32;
    new_shstr.extend_from_slice(name.as_bytes());
    new_shstr.push(0);

    let mut headers = file[shoff..sht_end].to_vec();

    let payload_off = align_up(file.len(), 8);
    file.resize(payload_off, 0);
    file.extend_from_slice(payload);

    let str_off = align_up(file.len(), 8);
    file.resize(str_off, 0);
    file.extend_from_slice(&new_shstr);

    put_u64_le(&mut headers, shstrndx * SHDR_SIZE + 24, str_off as u64);
    put_u64_le(&mut headers, shstrndx * SHDR_SIZE + 32, new_shstr.len() as u64);

    let mut new_shdr = [0u8; SHDR_SIZE];
    put_u32_le(&mut new_shdr, 0, name_off);
    put_u32_le(&mut new_shdr, 4, SHT_PROGBITS);
    put_u64_le(&mut new_shdr, 8, 0); // flags
    put_u64_le(&mut new_shdr, 16, 0); // addr
    put_u64_le(&mut new_shdr, 24, payload_off as u64);
    put_u64_le(&mut new_shdr, 32, payload.len() as u64);
    put_u32_le(&mut new_shdr, 40, 0); // link
    put_u32_le(&mut new_shdr, 44, 0); // info
    put_u64_le(&mut new_shdr, 48, 8); // addralign
    put_u64_le(&mut new_shdr, 56, 0); // entsize
    headers.extend_from_slice(&new_shdr);

    let new_shoff = align_up(file.len(), 8);
    file.resize(new_shoff, 0);
    file.extend_from_slice(&headers);

    put_u64_le(file, E_SHOFF, new_shoff as u64);
    put_u16_le(file, E_SHNUM, (shnum + 1) as u16);
    Ok(())
}
