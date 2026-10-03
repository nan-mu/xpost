//! Board `partitions.csv` → binary table for espflash / [`crate::client::flash::XdpTable`].
//!
//! XDP storage uses an application-defined partition **type** in the
//! ESP-IDF reserved range `0x40..=0xFE` (not `data` + numeric subtype).
//! That form is accepted by [`PartitionTable::try_from_str`].

use std::fs;
use std::path::Path;

use esp_idf_part::PartitionTable;

use crate::client::DEFAULT_IMAGE_CAPACITY;
use std::io;
use crate::client::flash::XdpTable;

/// Parse board `partitions.csv` into a binary partition table.
pub(crate) fn partition_table_bin(path: &Path) -> io::Result<Vec<u8>> {
    let table = load_partition_table(path)?;
    table
        .to_bin()
        .map_err(|e| std::io::Error::other(format!("partition to_bin: {e}")))
}

/// Resolve an [`XdpTable`] span from a named row in `partitions.csv`.
pub fn xdp_table_from_csv(path: &Path, partition: &str) -> io::Result<XdpTable> {
    let table = load_partition_table(path)?;
    let part = table.find(partition).ok_or_else(|| {
        std::io::Error::other(format!(
            "partitions.csv {}: no row named `{partition}`",
            path.display()
        ))
    })?;
    let len = part.size();
    if len == 0 {
        return Err(std::io::Error::other(format!(
            "partitions.csv `{partition}` Size must be > 0"
        )));
    }
    Ok(XdpTable {
        base: part.offset(),
        len,
        image_capacity: DEFAULT_IMAGE_CAPACITY,
    })
}

fn load_partition_table(path: &Path) -> io::Result<PartitionTable> {
    let text = fs::read_to_string(path)
        .map_err(|e| std::io::Error::other(format!("read partition table {}: {e}", path.display())))?;
    PartitionTable::try_from_str(text)
        .map_err(|e| std::io::Error::other(format!("partition table {}: {e}", path.display())))
}
