//! Parse libbpf-mcu `bpf.toml` maps into Compact ELF [`ImageMetadataV0`].

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use toml::Value;

use xpost_elf::{ImageMetadataV0, MAX_LAYOUT, MAX_MAPS, MAX_NAME, MapDescriptorV0};

/// Build metadata from a BTF/obj_report `bpf.toml`.
pub fn metadata_from_bpf_toml(
    path: &Path,
    name: &str,
    slot_address: u32,
    helper_address: u32,
    machine: u8,
) -> Result<ImageMetadataV0> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let root: Value =
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let maps = root.get("maps").cloned().unwrap_or(Value::Table(Default::default()));
    let Value::Table(map_table) = maps else {
        bail!("{}: maps must be TOML tables", path.display());
    };

    let mut records = heapless::Vec::<MapDescriptorV0, MAX_MAPS>::new();
    for (map_name, item) in &map_table {
        let Value::Table(fields) = item else {
            bail!("{}: map {map_name:?} must be a table", path.display());
        };
        match map_descriptor(map_name, fields)
            .with_context(|| format!("{}: map {map_name:?}", path.display()))?
        {
            None => continue,
            Some(rec) => records
                .push(rec)
                .map_err(|_| anyhow::anyhow!("{}: more than {MAX_MAPS} maps", path.display()))?,
        }
    }

    let meta = ImageMetadataV0 {
        slot_address,
        helper_address,
        machine,
        name: heapless_name(name)?,
        maps: records,
    };
    // Fail closed on metadata before the caller spends time on a large image.
    xpost_elf::build_container(&[0x01], &meta, None)
        .map_err(|e| anyhow::anyhow!("invalid metadata: {e:?}"))?;
    Ok(meta)
}

/// `Ok(None)` = map type outside Compact ELF V0 (HASH/ARRAY only); caller skips.
fn map_descriptor(
    map_name: &str,
    fields: &toml::map::Map<String, Value>,
) -> Result<Option<MapDescriptorV0>> {
    let Some(kind) = map_type_number(
        fields
            .get("map_type")
            .ok_or_else(|| anyhow::anyhow!("missing map_type"))?,
    )?
    else {
        eprintln!(
            "xpost-compact-elf: skip map {map_name:?} (not HASH/ARRAY in Compact ELF V0)"
        );
        return Ok(None);
    };
    let max_entries = int_field(fields, "max_entries")?;
    if !(1..=xpost_elf::MAX_ENTRIES).contains(&max_entries) {
        eprintln!(
            "xpost-compact-elf: skip map {map_name:?} (max_entries={max_entries} outside Compact ELF V0 1..{})",
            xpost_elf::MAX_ENTRIES
        );
        return Ok(None);
    }

    let key_bits = layout_bit_intervals(str_field(fields, "key")?)?;
    let value_bits = layout_bit_intervals(str_field(fields, "value")?)?;
    check_layout(&key_bits, 64)?;
    check_layout(&value_bits, 256)?;

    // Prefer explicit *_size; else derive from layout (older bpf.toml omit sizes).
    let key_size = size_bytes(fields, "key_size", &key_bits)?;
    let value_size = size_bytes(fields, "value_size", &value_bits)?;
    if key_bits.iter().map(|&b| usize::from(b)).sum::<usize>() != key_size * 8 {
        bail!("key layout does not match key_size");
    }
    if value_bits.iter().map(|&b| usize::from(b)).sum::<usize>() != value_size * 8 {
        bail!("value layout does not match value_size");
    }

    let key_layout = if kind == 2 {
        if key_bits.as_slice() != [32] {
            bail!("ARRAY requires an implicit u32 key");
        }
        None
    } else {
        Some(layout_vec(&key_bits)?)
    };
    let value_layout = Some(layout_vec(&value_bits)?);

    let vma = match (fields.get("map_vma"), fields.get("aot_addr")) {
        (Some(mv), Some(aa)) => {
            let a = toml_u32(mv)?;
            let b = toml_u32(aa)?;
            if a != b {
                bail!("map_vma disagrees with linked aot_addr");
            }
            a
        }
        (Some(mv), None) => toml_u32(mv)?,
        (None, Some(aa)) => toml_u32(aa)?,
        (None, None) => bail!("missing map_vma / aot_addr"),
    };

    Ok(Some(MapDescriptorV0 {
        name: heapless_name(map_name)?,
        map_type: kind,
        len: max_entries,
        map_vma: vma,
        key_layout,
        value_layout,
    }))
}

fn size_bytes(
    fields: &toml::map::Map<String, Value>,
    key: &str,
    bits: &[u8],
) -> Result<usize> {
    let from_layout = {
        let sum: usize = bits.iter().map(|&b| usize::from(b)).sum();
        if sum % 8 != 0 {
            bail!("{key}: layout bit sum {sum} is not byte-aligned");
        }
        sum / 8
    };
    match fields.get(key) {
        None => {
            if !(1..=256).contains(&from_layout) {
                bail!("{key}: derived size {from_layout} out of 1..256");
            }
            Ok(from_layout)
        }
        Some(v) => {
            let n = toml_u32(v)? as usize;
            if !(1..=256).contains(&n) {
                bail!("{key} must be in 1..256");
            }
            Ok(n)
        }
    }
}

/// Compact ELF V0 only encodes HASH=1 / ARRAY=2. Other Linux map types → `Ok(None)`.
fn map_type_number(v: &Value) -> Result<Option<u8>> {
    match v {
        Value::Integer(i) if *i == 1 || *i == 2 => Ok(Some(*i as u8)),
        Value::Integer(i) => {
            let _ = i;
            Ok(None)
        }
        Value::String(s) => match s.as_str() {
            "BPF_MAP_TYPE_HASH" | "HASH" => Ok(Some(1)),
            "BPF_MAP_TYPE_ARRAY" | "ARRAY" => Ok(Some(2)),
            _ => Ok(None),
        },
        _ => bail!("unknown map_type: {v:?}"),
    }
}

fn str_field<'a>(fields: &'a toml::map::Map<String, Value>, key: &str) -> Result<&'a str> {
    match fields.get(key) {
        Some(Value::String(s)) => Ok(s.as_str()),
        Some(other) => bail!("{key} must be a string, got {other:?}"),
        None => bail!("missing {key}"),
    }
}

fn int_field(fields: &toml::map::Map<String, Value>, key: &str) -> Result<u32> {
    match fields.get(key) {
        Some(v) => toml_u32(v),
        None => bail!("missing {key}"),
    }
}

fn toml_u32(v: &Value) -> Result<u32> {
    match v {
        Value::Integer(i) if *i >= 0 && *i <= i64::from(u32::MAX) => Ok(*i as u32),
        other => bail!("expected u32, got {other:?}"),
    }
}

/// One interval per field/array/padding line, measured in bits (Python `layout_bit_intervals`).
pub fn layout_bit_intervals(layout: &str) -> Result<Vec<u8>> {
    let mut intervals = Vec::new();
    for line in layout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.ends_with('{') || line == "}" {
            bail!("nested layout notation is unsupported: {line}");
        }
        let bits = parse_layout_line(line)?;
        if bits == 0 {
            bail!("layout interval must be positive");
        }
        if bits > 255 {
            bail!("layout interval bits must be 1..=255");
        }
        intervals.push(bits as u8);
    }
    Ok(intervals)
}

fn parse_layout_line(line: &str) -> Result<u32> {
    // [uif](\d+)\s+\w+(?:\[(\d+)\])?
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        bail!("unsupported layout line `{line}`");
    }
    let kind = bytes[0];
    if kind != b'u' && kind != b'i' && kind != b'f' {
        bail!("unsupported layout line `{line}`");
    }
    let mut i = 1;
    let mut width: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        width = width
            .checked_mul(10)
            .and_then(|w| w.checked_add(u32::from(bytes[i] - b'0')))
            .ok_or_else(|| anyhow::anyhow!("unsupported layout line `{line}`"))?;
        i += 1;
    }
    if width == 0 {
        bail!("unsupported layout line `{line}`");
    }
    if i >= bytes.len() || bytes[i] != b' ' {
        bail!("unsupported layout line `{line}`");
    }
    i += 1;
    let name_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    if i == name_start {
        bail!("unsupported layout line `{line}`");
    }
    let mut count = 1u32;
    if i < bytes.len() {
        if bytes[i] != b'[' {
            bail!("unsupported layout line `{line}`");
        }
        i += 1;
        let mut n: u32 = 0;
        let digit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            n = n
                .checked_mul(10)
                .and_then(|w| w.checked_add(u32::from(bytes[i] - b'0')))
                .ok_or_else(|| anyhow::anyhow!("unsupported layout line `{line}`"))?;
            i += 1;
        }
        if i == digit_start || i >= bytes.len() || bytes[i] != b']' {
            bail!("unsupported layout line `{line}`");
        }
        i += 1;
        count = n;
    }
    if i != bytes.len() {
        bail!("unsupported layout line `{line}`");
    }
    width
        .checked_mul(count)
        .ok_or_else(|| anyhow::anyhow!("unsupported layout line `{line}`"))
}

fn check_layout(bits: &[u8], max_bytes: usize) -> Result<()> {
    if !(1..=MAX_LAYOUT).contains(&bits.len()) {
        bail!("layout must have 1..32 descriptors; empty is not None");
    }
    let sum: usize = bits.iter().map(|&b| usize::from(b)).sum();
    if sum % 8 != 0 || sum > max_bytes * 8 {
        bail!("layout must span whole bytes and at most {max_bytes} bytes");
    }
    Ok(())
}

fn layout_vec(bits: &[u8]) -> Result<heapless::Vec<u8, MAX_LAYOUT>> {
    let mut v = heapless::Vec::new();
    v.extend_from_slice(bits)
        .map_err(|_| anyhow::anyhow!("layout exceeds MAX_LAYOUT"))?;
    Ok(v)
}

fn heapless_name(s: &str) -> Result<heapless::String<{ MAX_NAME + 1 }>> {
    heapless::String::try_from(s).map_err(|_| anyhow::anyhow!("name must be 1..{MAX_NAME} ASCII"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_layout_lines() {
        assert_eq!(layout_bit_intervals("u32 index").unwrap(), vec![32]);
        assert_eq!(
            layout_bit_intervals("u32 a\nu32 b").unwrap(),
            vec![32, 32]
        );
        assert!(layout_bit_intervals("u8 blob[32]").is_err());
    }
}
