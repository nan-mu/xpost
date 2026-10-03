//! Firmware + partition-table flash via [`crate::client::port::Downloader`].

use std::fs;
use std::path::Path;

use std::io;
use crate::client::flash::partitions;
use crate::client::port::Port;

/// Flash app ELF + partition table, then reset to app.
pub(crate) fn flash_fw(port: &Port, firmware: &Path, partitions_csv: &Path) -> io::Result<()> {
    let firmware = fs::read(firmware)
        .map_err(|e| std::io::Error::other(format!("read firmware {}: {e}", firmware.display())))?;
    if !partitions_csv.is_file() {
        return Err(std::io::Error::other(format!(
            "partition table not found: {}",
            partitions_csv.display()
        )));
    }
    let part_bin = partitions::partition_table_bin(partitions_csv)?;
    let mut dl = port.open_download()?;
    dl.flash_firmware(&firmware, &part_bin)?;
    dl.reset_to_app()?;
    Ok(())
}
