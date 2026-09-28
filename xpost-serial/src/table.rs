//! Cold-region XDP ELF table: list / install-by-name (overwrite or append).

use std::path::Path;

use xpost_elf::{ScanOutcome, scan_at};

use crate::error::{Error, Result};

/// just_drop default cold table (`TABLE` / `partitions.csv` `xdp`).
pub const JUST_DROP_TABLE: ColdTable = ColdTable {
    base: 0x310000,
    len: 0x10000,
    image_capacity: 0x1000,
};

/// Absolute flash span holding packed XDP ELF containers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColdTable {
    /// Absolute flash address of table start.
    pub base: u32,
    /// Table byte length.
    pub len: u32,
    /// Max single XDP ELF container size.
    pub image_capacity: usize,
}

/// One scanned XDP ELF in the cold table (offsets relative to [`ColdTable::base`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XdpEntry {
    pub name: String,
    pub offset: u32,
    pub len: u32,
}

/// Result of [`crate::Session::install_xdp`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Replaced an existing entry with the same name.
    Overwritten { offset: u32 },
    /// Appended after the last valid container.
    Created { offset: u32 },
}

/// Parse the XDP program name from an on-disk XDP ELF (host path).
pub fn xdp_name_from_file(path: &Path) -> Result<String> {
    let data = std::fs::read(path).map_err(|e| {
        Error::Msg(format!("read {}: {e}", path.display()))
    })?;
    xdp_name_from_bytes(&data)
}

/// Parse the XDP program name from XDP ELF bytes.
pub fn xdp_name_from_bytes(data: &[u8]) -> Result<String> {
    let mut src = data;
    match scan_at(&mut src, 0) {
        ScanOutcome::Ok(c) => Ok(c.name.as_str().to_string()),
        ScanOutcome::SoftSkip { .. } => Err(Error::Msg(
            "xdp.elf SoftSkip at 0 (not a valid XDP ELF V0 profile)".into(),
        )),
        ScanOutcome::HardFail => Err(Error::Msg(
            "xdp.elf HardFail at 0 (not a recognizable XDP ELF)".into(),
        )),
    }
}

/// Linear scan of a cold-table snapshot (relative offsets).
pub fn list_entries(table_bytes: &[u8]) -> Result<Vec<XdpEntry>> {
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

/// Decide install offset: overwrite same name, else append after last entry.
pub fn plan_install(
    entries: &[XdpEntry],
    name: &str,
    new_len: u32,
    table_len: u32,
) -> Result<InstallOutcome> {
    if let Some((idx, e)) = entries.iter().enumerate().find(|(_, e)| e.name == name) {
        let is_last = idx + 1 == entries.len();
        if !is_last && new_len != e.len {
            return Err(Error::Msg(format!(
                "overwrite `{name}` would resize mid-table ({new_len} != {}); unsupported",
                e.len
            )));
        }
        if is_last {
            let end = e.offset.saturating_add(new_len);
            if end > table_len {
                return Err(Error::Msg(format!(
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
        return Err(Error::Msg(format!(
            "append `{name}` needs {end:#x} bytes; table is {table_len:#x}"
        )));
    }
    Ok(InstallOutcome::Created { offset: free })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xpost_elf::{ImageMetadataV0, MapDescriptorV0, build_container};

    fn sample_elf(name: &str) -> Vec<u8> {
        let mut maps = heapless::Vec::new();
        maps.push(MapDescriptorV0 {
            name: heapless_name("rxcnt"),
            map_type: 2,
            len: 16,
            map_vma: 0x3fc9_b690,
            key_layout: None,
            value_layout: Some(heapless_layout(&[64])),
        })
        .unwrap();
        let meta = ImageMetadataV0 {
            slot_address: 0x3fc9_b5a4,
            helper_address: 0x4038_0000,
            machine: 1,
            name: heapless_name(name),
            maps,
        };
        build_container(&[0x01, 0x02, 0x03, 0x04], &meta, None)
            .unwrap()
            .as_slice()
            .to_vec()
    }

    fn heapless_name(s: &str) -> heapless::String<{ xpost_elf::MAX_NAME + 1 }> {
        let mut o = heapless::String::new();
        o.push_str(s).unwrap();
        o
    }

    fn heapless_layout(bits: &[u8]) -> heapless::Vec<u8, { xpost_elf::MAX_LAYOUT }> {
        let mut v = heapless::Vec::new();
        for &b in bits {
            v.push(b).unwrap();
        }
        v
    }

    #[test]
    fn name_from_bytes_and_list() {
        let a = sample_elf("just_drop");
        assert_eq!(xdp_name_from_bytes(&a).unwrap(), "just_drop");
        let mut table = a.clone();
        table.resize(0x2000, 0xFF);
        let entries = list_entries(&table).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "just_drop");
        assert_eq!(entries[0].offset, 0);
    }

    #[test]
    fn plan_overwrite_and_create() {
        let a = sample_elf("just_drop");
        let entries = list_entries(&a).unwrap();
        let over = plan_install(&entries, "just_drop", a.len() as u32, 0x10000).unwrap();
        assert_eq!(over, InstallOutcome::Overwritten { offset: 0 });
        let created = plan_install(&entries, "other", 100, 0x10000).unwrap();
        assert_eq!(
            created,
            InstallOutcome::Created {
                offset: entries[0].len
            }
        );
    }
}
