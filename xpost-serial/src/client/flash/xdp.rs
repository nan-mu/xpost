//! XDP ELF table ops: list / install-by-name / clear (span from `partitions.csv`).

use std::path::Path;

use xpost_elf::{ScanOutcome, scan_at};

use crate::client::FLASH_SECTOR;
use std::io;
use crate::client::flash::FlashConfig;
use crate::client::port::Port;

/// Absolute flash span holding packed XDP ELF containers (`partitions.csv` row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XdpTable {
    pub base: u32,
    pub len: u32,
    pub image_capacity: usize,
}

/// One scanned XDP ELF in the XDP table (offsets relative to [`XdpTable::base`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdpEntry {
    pub name: String,
    pub offset: u32,
    pub len: u32,
}

/// Result of a name-based install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    Overwritten { offset: u32 },
    Created { offset: u32 },
}

pub fn xdp_name_from_file(path: &Path) -> io::Result<String> {
    let data =
        std::fs::read(path).map_err(|e| std::io::Error::other(format!("read {}: {e}", path.display())))?;
    xdp_name_from_bytes(&data)
}

pub fn xdp_name_from_bytes(data: &[u8]) -> io::Result<String> {
    let mut src = data;
    match scan_at(&mut src, 0) {
        ScanOutcome::Ok(c) => Ok(c.name.as_str().to_string()),
        ScanOutcome::SoftSkip { .. } => Err(std::io::Error::other(
            "xdp.elf SoftSkip at 0 (not a valid XDP ELF V0 profile)",
        )),
        ScanOutcome::HardFail => Err(std::io::Error::other(
            "xdp.elf HardFail at 0 (not a recognizable XDP ELF)",
        )),
    }
}

pub fn list_entries(table_bytes: &[u8]) -> io::Result<Vec<XdpEntry>> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    let mut src = table_bytes;
    while cursor.saturating_add(xpost_elf::FIXED_WIRE_SIZE) <= src.len() {
        match scan_at(&mut src, cursor) {
            ScanOutcome::HardFail => break,
            ScanOutcome::SoftSkip { file_len } => {
                if file_len == 0 {
                    break;
                }
                cursor = cursor.saturating_add(file_len);
            }
            ScanOutcome::Ok(parsed) => {
                let file_len = parsed.file_len;
                if file_len == 0 {
                    break;
                }
                out.push(XdpEntry {
                    name: parsed.name.as_str().to_string(),
                    offset: cursor as u32,
                    len: file_len as u32,
                });
                cursor = cursor.saturating_add(file_len);
            }
        }
    }
    Ok(out)
}

pub fn plan_install(
    entries: &[XdpEntry],
    name: &str,
    new_len: u32,
    table_len: u32,
) -> io::Result<InstallOutcome> {
    if let Some((idx, e)) = entries.iter().enumerate().find(|(_, e)| e.name == name) {
        let is_last = idx + 1 == entries.len();
        if !is_last && new_len != e.len {
            return Err(std::io::Error::other(format!(
                "overwrite `{name}` would resize mid-table ({new_len} != {}); unsupported",
                e.len
            )));
        }
        if is_last {
            let end = e.offset.saturating_add(new_len);
            if end > table_len {
                return Err(std::io::Error::other(format!(
                    "overwrite `{name}` needs {end:#x} bytes; table is {table_len:#x}"
                )));
            }
        }
        return Ok(InstallOutcome::Overwritten { offset: e.offset });
    }
    let free = entries
        .last()
        .map(|e| e.offset.saturating_add(e.len))
        .unwrap_or(0);
    let end = free.saturating_add(new_len);
    if end > table_len {
        return Err(std::io::Error::other(format!(
            "append `{name}` needs {end:#x} bytes; table is {table_len:#x}"
        )));
    }
    Ok(InstallOutcome::Created { offset: free })
}

pub(crate) fn install(
    port: &Port,
    cfg: &FlashConfig,
    name: &str,
    path: &Path,
) -> io::Result<InstallOutcome> {
    let xdp = cfg.xdp_table()?;
    let data =
        std::fs::read(path).map_err(|e| std::io::Error::other(format!("read {}: {e}", path.display())))?;
    let cap = xdp.image_capacity;
    if data.len() > cap {
        return Err(std::io::Error::other(format!(
            "image too large: {} bytes > capacity {cap}",
            data.len()
        )));
    }
    let file_name = xdp_name_from_bytes(&data)?;
    if file_name != name {
        return Err(std::io::Error::other(format!(
            "xdp name mismatch: arg `{name}`, file `{file_name}`"
        )));
    }
    let new_len = data.len() as u32;
    let mut dl = port.open_download()?;
    let table = dl.read_flash(xdp.base, xdp.len)?;
    let entries = list_entries(&table)?;
    let outcome = plan_install(&entries, name, new_len, xdp.len)?;
    let (rel, old_len) = match outcome {
        InstallOutcome::Overwritten { offset } => {
            let old = entries
                .iter()
                .find(|e| e.offset == offset)
                .map(|e| e.len)
                .unwrap_or(new_len);
            (offset, old)
        }
        InstallOutcome::Created { offset } => (offset, new_len),
    };
    let abs = xdp.base.saturating_add(rel);
    if !abs.is_multiple_of(FLASH_SECTOR) {
        return Err(std::io::Error::other(format!(
            "xdp install `{name}` at {abs:#x} is not {FLASH_SECTOR:#x}-aligned \
             (clear the cold table before appending a different name)"
        )));
    }
    dl.reconnect()?;
    let erase_len = {
        let need = old_len.max(new_len);
        need.saturating_add(FLASH_SECTOR - 1) / FLASH_SECTOR * FLASH_SECTOR
    };
    if erase_len > 0 {
        dl.erase_region(abs, erase_len)?;
    }
    dl.write_bin_verified(abs, &data)?;
    dl.reset_to_app()?;
    Ok(outcome)
}

/// `names` empty ⇒ erase the entire XDP table.
pub(crate) fn clear(port: &Port, cfg: &FlashConfig, names: &[String]) -> io::Result<()> {
    let xdp = cfg.xdp_table()?;
    let mut dl = port.open_download()?;
    if !names.is_empty() {
        let table = dl.read_flash(xdp.base, xdp.len)?;
        let entries = list_entries(&table)?;
        dl.reconnect()?;
        for name in names {
            let Some(e) = entries.iter().find(|e| e.name == *name) else {
                return Err(std::io::Error::other(format!("xdp `{name}` not found")));
            };
            let abs = xdp.base.saturating_add(e.offset);
            let erase_len = e.len.saturating_add(FLASH_SECTOR - 1) / FLASH_SECTOR * FLASH_SECTOR;
            if erase_len > 0 {
                dl.erase_region(abs, erase_len)?;
            }
        }
        dl.reset_to_app()?;
        return Ok(());
    }
    let abs = xdp.base;
    let len = xdp.len;
    if !abs.is_multiple_of(FLASH_SECTOR) || !len.is_multiple_of(FLASH_SECTOR) || len == 0 {
        return Err(std::io::Error::other(format!(
            "xdp table {abs:#x}+{len:#x} must be non-empty and {FLASH_SECTOR:#x}-aligned"
        )));
    }
    dl.erase_region(abs, len)?;
    dl.reset_to_app()?;
    Ok(())
}

pub(crate) fn list(port: &Port, cfg: &FlashConfig) -> io::Result<Vec<XdpEntry>> {
    let xdp = cfg.xdp_table()?;
    let mut dl = port.open_download()?;
    let table = dl.read_flash(xdp.base, xdp.len)?;
    let entries = list_entries(&table)?;
    dl.reset_to_app()?;
    Ok(entries)
}
