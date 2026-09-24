//! Tolerant `partitions.csv` → binary table for espflash.
//!
//! `esp-idf-part` 0.6 panics on `Type::Data` + numeric custom subtype when
//! *parsing* (CSV or binary round-trip), e.g. just_drop `xdp, data, 0x40`.
//! We build [`Partition`] values with [`SubType::Custom`] and emit `.bin` via
//! `to_bin()` (write path is fine). [`IdfBootloaderFormat`] still cannot load
//! that bin, so [`crate::session::Session::program`] flashes the app with the
//! default table then overwrites the on-device table at [`PART_TABLE_OFFSET`].

use std::fs;
use std::path::Path;
use std::str::FromStr;

use esp_idf_part::{AppType, DataType, Flags, Partition, PartitionTable, SubType, Type};

use crate::error::{Error, Result};

/// Default ESP-IDF partition-table flash offset (ESP32-C3).
pub(crate) const PART_TABLE_OFFSET: u32 = 0x8000;

/// Parse board `partitions.csv` into a binary partition table (correct subtypes).
pub(crate) fn partition_table_bin(path: &Path) -> Result<Vec<u8>> {
    let text = fs::read_to_string(path).map_err(|e| {
        Error::Msg(format!("read partition table {}: {e}", path.display()))
    })?;
    let table = parse_partitions_csv(&text)?;
    table
        .to_bin()
        .map_err(|e| Error::Msg(format!("partition to_bin: {e}")))
}

fn parse_partitions_csv(text: &str) -> Result<PartitionTable> {
    let mut parts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.to_ascii_lowercase().starts_with("name,") {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if cols.len() < 5 {
            continue;
        }
        let name = cols[0];
        let ty = parse_type(cols[1])?;
        let subtype = parse_subtype(ty, cols[2])?;
        let offset = parse_u32(cols[3]).ok_or_else(|| {
            Error::Msg(format!("partition `{name}`: bad offset `{}`", cols[3]))
        })?;
        let size = parse_u32(cols[4]).ok_or_else(|| {
            Error::Msg(format!("partition `{name}`: bad size `{}`", cols[4]))
        })?;
        let flags = if cols.len() > 5 && !cols[5].is_empty() {
            parse_flags(cols[5])?
        } else {
            Flags::empty()
        };
        parts.push(Partition::new(name, ty, subtype, offset, size, flags));
    }
    if parts.is_empty() {
        return Err(Error::Msg("partition table has no rows".into()));
    }
    Ok(PartitionTable::new(parts))
}

fn parse_type(s: &str) -> Result<Type> {
    let lower = s.to_ascii_lowercase();
    if lower == "app" {
        return Ok(Type::App);
    }
    if lower == "data" {
        return Ok(Type::Data);
    }
    if let Some(v) = parse_u32(s) {
        if v == 0 {
            return Ok(Type::App);
        }
        if v == 1 {
            return Ok(Type::Data);
        }
        if v <= u8::MAX as u32 {
            return Ok(Type::Custom(v as u8));
        }
    }
    Err(Error::Msg(format!("invalid partition type `{s}`")))
}

fn parse_subtype(ty: Type, s: &str) -> Result<SubType> {
    let lower = s.to_ascii_lowercase();
    match ty {
        Type::App => {
            if let Ok(app) = AppType::from_str(&lower) {
                return Ok(SubType::App(app));
            }
            if let Some(v) = parse_u32(s) {
                if let Some(app) = AppType::from_repr(v as usize) {
                    return Ok(SubType::App(app));
                }
                if v <= u8::MAX as u32 {
                    return Ok(SubType::Custom(v as u8));
                }
            }
            Err(Error::Msg(format!("invalid app subtype `{s}`")))
        }
        Type::Data => {
            if let Ok(data) = DataType::from_str(&lower) {
                return Ok(SubType::Data(data));
            }
            if let Some(v) = parse_u32(s) {
                if let Some(data) = DataType::from_repr(v as usize) {
                    return Ok(SubType::Data(data));
                }
                if v <= u8::MAX as u32 {
                    // Custom data subtype (ESP-IDF 0x40..=0xFE), e.g. just_drop xdp.
                    return Ok(SubType::Custom(v as u8));
                }
            }
            Err(Error::Msg(format!("invalid data subtype `{s}`")))
        }
        Type::Custom(_) => {
            if let Some(v) = parse_u32(s) {
                if v <= u8::MAX as u32 {
                    return Ok(SubType::Custom(v as u8));
                }
            }
            Err(Error::Msg(format!("invalid custom subtype `{s}`")))
        }
    }
}

fn parse_flags(s: &str) -> Result<Flags> {
    let mut flags = Flags::empty();
    for part in s.split(':') {
        let p = part.trim().to_ascii_lowercase();
        if p.is_empty() {
            continue;
        }
        if p == "encrypted" {
            flags |= Flags::ENCRYPTED;
        } else if p == "readonly" {
            flags |= Flags::READONLY;
        } else {
            return Err(Error::Msg(format!("unknown partition flag `{part}`")));
        }
    }
    Ok(flags)
}

fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).ok();
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn just_drop_xdp_custom_subtype_to_bin() {
        let csv = "\
# Name, Type, SubType, Offset, Size, Flags
nvs, data, nvs, 0x9000, 0x6000,
phy_init, data, phy, 0xf000, 0x1000,
factory, app, factory, 0x10000, 0x300000,
xdp, data, 0x40, 0x310000, 0x10000,
";
        let table = parse_partitions_csv(csv).unwrap();
        let xdp = table.find("xdp").expect("xdp row");
        assert_eq!(xdp.offset(), 0x310000);
        assert_eq!(xdp.size(), 0x10000);
        assert_eq!(xdp.subtype(), SubType::Custom(0x40));
        let bin = table.to_bin().unwrap();
        assert_eq!(&bin[..2], &[0xAA, 0x50]);
        assert_eq!(bin.len(), 0xC00); // esp-idf-part MAX_PARTITION_LENGTH
    }
}
