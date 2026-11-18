//! XDP ELF table ops: list / install-by-name / clear (span from `partitions.csv`).

use std::path::Path;

use xpost_elf::{ScanOutcome, scan_at};

use crate::client::FLASH_SECTOR;
use crate::client::FirmwareExpectation;
use crate::client::flash::FlashConfig;
use crate::client::port::download::PART_TABLE_OFFSET;
use crate::client::port::{Downloader, Port};
use sha2::{Digest, Sha256};
use std::io;

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
    let data = std::fs::read(path)
        .map_err(|e| std::io::Error::other(format!("read {}: {e}", path.display())))?;
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
            ScanOutcome::HardFail => match xpost_elf::skip_erased_gap(cursor) {
                Some(next) => cursor = next,
                None => break,
            },
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

fn align_sector(off: u32) -> u32 {
    let a = FLASH_SECTOR;
    off.saturating_add(a - 1) & !(a - 1)
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
    let free = align_sector(
        entries
            .last()
            .map(|e| e.offset.saturating_add(e.len))
            .unwrap_or(0),
    );
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
    expectation: &FirmwareExpectation,
) -> io::Result<InstallOutcome> {
    let xdp = cfg.xdp_table()?;
    let data = std::fs::read(path)
        .map_err(|e| std::io::Error::other(format!("read {}: {e}", path.display())))?;
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
    let mut dl = port.open_download()?;
    let result = install_verified(&mut *dl, xdp, &cfg.partition, name, &data, expectation);
    let reset = dl.reset_to_app();
    match result {
        Ok(outcome) => {
            reset?;
            Ok(outcome)
        }
        Err(error) => Err(error),
    }
}

trait FlashIo {
    fn application_segment(&mut self, elf: &[u8]) -> io::Result<(u32, Vec<u8>)>;
    fn read_flash(&mut self, addr: u32, len: u32) -> io::Result<Vec<u8>>;
    fn reconnect(&mut self) -> io::Result<()>;
    fn erase_region(&mut self, addr: u32, len: u32) -> io::Result<()>;
    fn write_bin_verified(&mut self, addr: u32, data: &[u8]) -> io::Result<()>;
}
impl FlashIo for Downloader {
    fn application_segment(&mut self, elf: &[u8]) -> io::Result<(u32, Vec<u8>)> {
        Downloader::application_segment(self, elf)
    }
    fn read_flash(&mut self, addr: u32, len: u32) -> io::Result<Vec<u8>> {
        Downloader::read_flash(self, addr, len)
    }
    fn reconnect(&mut self) -> io::Result<()> {
        Downloader::reconnect(self)
    }
    fn erase_region(&mut self, addr: u32, len: u32) -> io::Result<()> {
        Downloader::erase_region(self, addr, len)
    }
    fn write_bin_verified(&mut self, addr: u32, data: &[u8]) -> io::Result<()> {
        Downloader::write_bin_verified(self, addr, data)
    }
}

fn install_verified(
    io: &mut impl FlashIo,
    xdp: XdpTable,
    partition: &str,
    name: &str,
    data: &[u8],
    expectation: &FirmwareExpectation,
) -> io::Result<InstallOutcome> {
    let new_len = data.len() as u32;
    let segment = io.application_segment(&expectation.elf)?;
    verify_firmware(expectation, xdp, partition, &segment, |addr, len| {
        io.read_flash(addr, len)
    })?;
    let table = io.read_flash(xdp.base, xdp.len)?;
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
    io.reconnect()?;
    let erase_len = {
        let need = old_len.max(new_len);
        need.saturating_add(FLASH_SECTOR - 1) / FLASH_SECTOR * FLASH_SECTOR
    };
    if erase_len > 0 {
        io.erase_region(abs, erase_len)?;
    }
    io.write_bin_verified(abs, &data)?;
    Ok(outcome)
}

fn verify_firmware(
    expectation: &FirmwareExpectation,
    xdp: XdpTable,
    partition: &str,
    app: &(u32, Vec<u8>),
    mut read: impl FnMut(u32, u32) -> io::Result<Vec<u8>>,
) -> io::Result<()> {
    let fail = || io::Error::other("unknown or mismatched firmware layout");
    if <[u8; 32]>::from(Sha256::digest(&expectation.elf)) != expectation.sha256 {
        return Err(io::Error::other("firmware snapshot SHA256 mismatch"));
    }
    let bytes = &expectation.partition_table;
    if xdp.len == 0
        || !xdp.base.is_multiple_of(FLASH_SECTOR)
        || !xdp.len.is_multiple_of(FLASH_SECTOR)
        || bytes.is_empty()
        || bytes.len() > 0x1000
        || bytes.len() % 32 != 0
    {
        return Err(fail());
    }
    let parsed =
        std::panic::catch_unwind(|| esp_idf_part::PartitionTable::try_from_bytes(bytes.clone()));
    if !matches!(parsed, Ok(Ok(_))) {
        return Err(fail());
    }
    let mut ranges = Vec::new();
    let mut factory = None;
    let mut found_xdp = false;
    let mut terminated = false;
    for row in bytes.chunks_exact(32) {
        if row[0..2] != [0xaa, 0x50] {
            if row[0..2] == [0xeb, 0xeb] || row.iter().all(|b| *b == 0xff) {
                terminated = true;
                break;
            }
            return Err(fail());
        }
        let start = u32::from_le_bytes(row[4..8].try_into().unwrap());
        let len = u32::from_le_bytes(row[8..12].try_into().unwrap());
        let end = start.checked_add(len).ok_or_else(fail)?;
        if len == 0 || start < 0x9000 || ranges.iter().any(|&(a, b)| start < b && a < end) {
            return Err(fail());
        }
        ranges.push((start, end));
        if row[2] == 1 && row[3] == 0 {
            return Err(fail());
        }
        if row[2] == 0 {
            if row[3] != 0 || factory.replace((start, end)).is_some() {
                return Err(fail());
            }
        }
        let label_end = row[12..28].iter().position(|b| *b == 0).unwrap_or(16);
        if &row[12..12 + label_end] == partition.as_bytes() {
            if found_xdp || !(0x40..=0xfe).contains(&row[2]) || start != xdp.base || len != xdp.len
            {
                return Err(fail());
            }
            found_xdp = true;
        }
    }
    let (start, end) = factory.ok_or_else(fail)?;
    let app_end = app
        .0
        .checked_add(u32::try_from(app.1.len()).map_err(|_| fail())?)
        .ok_or_else(fail)?;
    if !terminated || !found_xdp || app.1.is_empty() || app.0 != start || app_end > end {
        return Err(fail());
    }
    if read(PART_TABLE_OFFSET, bytes.len() as u32)? != *bytes {
        return Err(io::Error::other("firmware partition table mismatch"));
    }
    if read(app.0, app.1.len() as u32)? != app.1 {
        return Err(io::Error::other("firmware application mismatch"));
    }
    Ok(())
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

#[cfg(test)]
mod attestation_tests {
    use super::*;
    use crate::client::port::download::{firmware_image, tests::riscv_elf};
    use espflash::target::{Chip, XtalFrequency};

    struct Fake {
        table: Vec<u8>,
        app: Vec<u8>,
        events: Vec<&'static str>,
        fail_read: Option<&'static str>,
        fail_reconnect: bool,
        fail_write: bool,
    }
    impl FlashIo for Fake {
        fn application_segment(&mut self, elf: &[u8]) -> io::Result<(u32, Vec<u8>)> {
            let mut segments =
                firmware_image(elf, Chip::Esp32c3, XtalFrequency::_40Mhz)?.ota_segments();
            if segments.len() != 1 {
                return Err(io::Error::other("expected one application segment"));
            }
            let segment = segments.pop().unwrap();
            Ok((segment.addr, segment.data.into_owned()))
        }
        fn read_flash(&mut self, addr: u32, len: u32) -> io::Result<Vec<u8>> {
            self.events.push(if addr == PART_TABLE_OFFSET {
                "partition"
            } else if addr == 0x10000 {
                "app"
            } else {
                "cold"
            });
            if self.fail_read == self.events.last().copied() {
                return Err(io::Error::other("read failed"));
            }
            Ok(if addr == PART_TABLE_OFFSET {
                self.table.clone()
            } else if addr == 0x10000 {
                self.app.clone()
            } else {
                vec![0xff; len as usize]
            })
        }
        fn reconnect(&mut self) -> io::Result<()> {
            self.events.push("reconnect");
            if self.fail_reconnect {
                return Err(io::Error::other("reconnect failed"));
            }
            Ok(())
        }
        fn erase_region(&mut self, _: u32, _: u32) -> io::Result<()> {
            self.events.push("erase");
            Ok(())
        }
        fn write_bin_verified(&mut self, _: u32, _: &[u8]) -> io::Result<()> {
            self.events.push("write");
            if self.fail_write {
                return Err(io::Error::other("write failed"));
            }
            Ok(())
        }
    }
    fn fixture() -> (FirmwareExpectation, XdpTable, Fake) {
        let table = esp_idf_part::PartitionTable::try_from_str("nvs,data,nvs,0x9000,0x6000,\nfactory,app,factory,0x10000,0x300000,\nxdp,0x40,0,0x310000,0x10000,\n").unwrap().to_bin().unwrap();
        let elf = riscv_elf();
        let app = firmware_image(&elf, Chip::Esp32c3, XtalFrequency::_40Mhz)
            .unwrap()
            .ota_segments()
            .pop()
            .unwrap()
            .data
            .into_owned();
        let expectation = FirmwareExpectation {
            sha256: Sha256::digest(&elf).into(),
            elf,
            partition_table: table.clone(),
        };
        let xdp = XdpTable {
            base: 0x310000,
            len: 0x10000,
            image_capacity: 4096,
        };
        (
            expectation,
            xdp,
            Fake {
                table,
                app,
                events: Vec::new(),
                fail_read: None,
                fail_reconnect: false,
                fail_write: false,
            },
        )
    }
    #[test]
    fn matching_firmware_precedes_cold_read_and_write_on_every_attempt() {
        let (expectation, xdp, mut io) = fixture();
        for _ in 0..2 {
            install_verified(&mut io, xdp, "xdp", "test", &[7], &expectation).unwrap();
        }
        assert_eq!(
            io.events,
            [
                "partition",
                "app",
                "cold",
                "reconnect",
                "erase",
                "write",
                "partition",
                "app",
                "cold",
                "reconnect",
                "erase",
                "write"
            ]
        );
    }
    #[test]
    fn failed_write_does_not_skip_verification_on_retry() {
        let (expectation, xdp, mut io) = fixture();
        io.fail_write = true;
        assert!(install_verified(&mut io, xdp, "xdp", "test", &[7], &expectation).is_err());
        io.fail_write = false;
        io.app[0] ^= 1;
        io.events.clear();
        assert!(install_verified(&mut io, xdp, "xdp", "test", &[7], &expectation).is_err());
        assert_eq!(io.events, ["partition", "app"]);
    }

    #[test]
    fn failures_never_reach_cold_flash() {
        for kind in 0..7 {
            let (mut expectation, mut xdp, mut io) = fixture();
            match kind {
                0 => io.app[0] ^= 1,
                1 => io.table[0] ^= 1,
                2 => io.fail_read = Some("partition"),
                3 => expectation.sha256[0] ^= 1,
                4 => expectation.partition_table[0] ^= 1,
                5 => xdp.base += 0x1000,
                _ => expectation.elf.clear(),
            }
            assert!(install_verified(&mut io, xdp, "xdp", "test", &[7], &expectation).is_err());
            assert!(
                !io.events
                    .iter()
                    .any(|e| ["cold", "erase", "write"].contains(e))
            );
        }
    }
    #[test]
    fn read_reconnect_and_layout_failures_never_write_cold_flash() {
        for kind in 0..7 {
            let (mut expectation, xdp, mut io) = fixture();
            match kind {
                0 => io.fail_read = Some("app"),
                1 => io.fail_reconnect = true,
                // OTA app subtype, duplicate factory, overlapping XDP, duplicate XDP,
                // or an application too large for the factory partition.
                2 => expectation.partition_table[32 + 3] = 0x10,
                3 => {
                    let row = expectation.partition_table[32..64].to_vec();
                    expectation.partition_table[96..128].copy_from_slice(&row);
                    expectation.partition_table[100..104]
                        .copy_from_slice(&0x320000u32.to_le_bytes());
                }
                4 => expectation.partition_table[8..12].copy_from_slice(&0x8000u32.to_le_bytes()),
                5 => {
                    let row = expectation.partition_table[64..96].to_vec();
                    expectation.partition_table[96..128].copy_from_slice(&row);
                }
                _ => expectation.partition_table[32 + 8..32 + 12]
                    .copy_from_slice(&1u32.to_le_bytes()),
            }
            if kind >= 2 {
                let end = if kind == 3 || kind == 5 { 128 } else { 96 };
                expectation.partition_table[end..].fill(0xff);
                io.table = expectation.partition_table.clone();
                if [2, 6].contains(&kind) {
                    assert!(
                        esp_idf_part::PartitionTable::try_from_bytes(
                            expectation.partition_table.clone()
                        )
                        .is_ok(),
                        "fixture {kind}"
                    );
                }
            }
            assert!(
                install_verified(&mut io, xdp, "xdp", "test", &[7], &expectation).is_err(),
                "case {kind}"
            );
            assert!(
                !io.events.iter().any(|e| ["erase", "write"].contains(e)),
                "case {kind}"
            );
            if kind == 0 {
                assert_eq!(io.events, ["partition", "app"]);
            }
            if kind == 1 {
                assert_eq!(io.events, ["partition", "app", "cold", "reconnect"]);
            }
        }
    }
}
