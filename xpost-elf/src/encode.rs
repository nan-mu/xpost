//! Compact ELF V0 encoder. Wire layout matches `docs/xpost_compact_elf.proto`
//! and the Python reference in `libbpf-mcu/make/xpost_elf.py`.

use sha2::{Digest, Sha256};

use crate::{MAX_LAYOUT, MAX_MAPS, MAX_NAME, align4};

/// Complete file size limit (ESP32-C3 profile).
pub const MAX_CONTAINER_SIZE: usize = 4096;
/// `.xpost.meta` size limit.
pub const MAX_META_SIZE: usize = 1024;
/// Map capacity profile limit.
pub const MAX_ENTRIES: u32 = 256;

const ELF32_EHDR_SIZE: u16 = 52;
const ELF32_SHDR_SIZE: usize = 40;
const SHT_COUNT: usize = 4;
const META_OFFSET: usize = 52 + SHT_COUNT * ELF32_SHDR_SIZE;
const IMAGE_DIGEST_SIZE: usize = 32;
const XPOST_VERSION: u8 = 0;
const SHT_XPOST_META: u32 = 0x8000_0001;
const SHT_XPOST_IMAGE: u32 = 0x8000_0002;

/// Fixed `.shstrtab` bytes (36).
pub const SECTION_NAMES: &[u8] = b"\0.xpost.meta\0.xpost.image\0.shstrtab\0";

const IDENT: [u8; 16] = [0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// One MapDescriptorV0 on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDescriptorV0 {
    pub name: heapless::String<{ MAX_NAME + 1 }>,
    pub map_type: u8,
    pub len: u32,
    pub map_vma: u32,
    pub key_layout: Option<heapless::Vec<u8, MAX_LAYOUT>>,
    pub value_layout: Option<heapless::Vec<u8, MAX_LAYOUT>>,
}

/// Image metadata used by [`build_container`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageMetadataV0 {
    pub slot_address: u32,
    pub helper_address: u32,
    pub machine: u8,
    pub name: heapless::String<{ MAX_NAME + 1 }>,
    pub maps: heapless::Vec<MapDescriptorV0, MAX_MAPS>,
}

/// Encode / validation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    EmptyImage,
    BadName,
    BadAddress,
    BadMachine,
    BadMapType,
    BadMapLen,
    BadLayout,
    BadHashArrayCombo,
    DuplicateMapName,
    DuplicateMapVma,
    MetaTooLarge,
    ContainerTooLarge,
    ImageExtentOverflow,
    ImageExceedsSlot,
    TooManyMaps,
    BufferFull,
}

/// Encode a Compact ELF V0 container.
///
/// When `max_image_len` is `Some(n)`, fails if `image.len() > n` (slot fit check).
pub fn build_container(
    image: &[u8],
    metadata: &ImageMetadataV0,
    max_image_len: Option<u32>,
) -> Result<heapless::Vec<u8, MAX_CONTAINER_SIZE>, EncodeError> {
    if image.is_empty() {
        return Err(EncodeError::EmptyImage);
    }
    if let Some(max) = max_image_len
        && (image.len() as u32) > max
    {
        return Err(EncodeError::ImageExceedsSlot);
    }

    let meta = encode_metadata(metadata)?;
    let image_off = META_OFFSET + meta.len();
    let names_off = image_off + image.len();
    let digest_off = align4(names_off + SECTION_NAMES.len());
    let file_len = digest_off
        .checked_add(IMAGE_DIGEST_SIZE)
        .ok_or(EncodeError::ContainerTooLarge)?;
    if file_len > MAX_CONTAINER_SIZE {
        return Err(EncodeError::ContainerTooLarge);
    }
    let slot_end = (metadata.slot_address as u64)
        .checked_add(image.len() as u64)
        .ok_or(EncodeError::ImageExtentOverflow)?;
    if slot_end > 0x1_0000_0000 {
        return Err(EncodeError::ImageExtentOverflow);
    }

    let mut out = heapless::Vec::<u8, MAX_CONTAINER_SIZE>::new();
    push_ehdr(&mut out)?;
    push_shdr(&mut out, [0; 10])?;
    push_shdr(
        &mut out,
        [
            1,
            SHT_XPOST_META,
            0,
            0,
            META_OFFSET as u32,
            meta.len() as u32,
            2,
            0,
            4,
            0,
        ],
    )?;
    push_shdr(
        &mut out,
        [
            13,
            SHT_XPOST_IMAGE,
            6,
            metadata.slot_address,
            image_off as u32,
            image.len() as u32,
            0,
            0,
            4,
            0,
        ],
    )?;
    push_shdr(&mut out, [26, 3, 0, 0, names_off as u32, 36, 0, 0, 1, 0])?;

    extend(&mut out, &meta)?;
    extend(&mut out, image)?;
    extend(&mut out, SECTION_NAMES)?;
    while !out.len().is_multiple_of(4) {
        out.push(0).map_err(|_| EncodeError::BufferFull)?;
    }
    let digest = Sha256::digest(image);
    extend(&mut out, &digest)?;
    debug_assert_eq!(out.len(), file_len);
    Ok(out)
}

fn encode_metadata(
    metadata: &ImageMetadataV0,
) -> Result<heapless::Vec<u8, MAX_META_SIZE>, EncodeError> {
    check_address(metadata.slot_address)?;
    check_address(metadata.helper_address)?;
    if metadata.machine != 1 {
        return Err(EncodeError::BadMachine);
    }
    if metadata.maps.len() > MAX_MAPS {
        return Err(EncodeError::TooManyMaps);
    }
    check_name(metadata.name.as_str())?;

    for (i, a) in metadata.maps.iter().enumerate() {
        for (j, b) in metadata.maps.iter().enumerate() {
            if i >= j {
                continue;
            }
            if a.name == b.name {
                return Err(EncodeError::DuplicateMapName);
            }
            if a.map_vma == b.map_vma {
                return Err(EncodeError::DuplicateMapVma);
            }
        }
    }

    let mut data = heapless::Vec::<u8, MAX_META_SIZE>::new();
    data.push(XPOST_VERSION)
        .map_err(|_| EncodeError::BufferFull)?;
    data.push(metadata.machine)
        .map_err(|_| EncodeError::BufferFull)?;
    push_u16le(&mut data, metadata.maps.len() as u16)?;
    push_u32le(&mut data, metadata.helper_address)?;
    push_cstring4(&mut data, metadata.name.as_str())?;
    for m in &metadata.maps {
        encode_map(&mut data, m)?;
    }
    if data.len() > MAX_META_SIZE {
        return Err(EncodeError::MetaTooLarge);
    }
    Ok(data)
}

fn encode_map(
    out: &mut heapless::Vec<u8, MAX_META_SIZE>,
    item: &MapDescriptorV0,
) -> Result<(), EncodeError> {
    check_name(item.name.as_str())?;
    if item.map_type != 1 && item.map_type != 2 {
        return Err(EncodeError::BadMapType);
    }
    if !(1..=MAX_ENTRIES).contains(&item.len) {
        return Err(EncodeError::BadMapLen);
    }
    check_address(item.map_vma)?;

    let key = layout_bytes(item.key_layout.as_ref(), 64)?;
    let value = layout_bytes(item.value_layout.as_ref(), 256)?;
    if value.is_empty()
        || (item.map_type == 1 && key.is_empty())
        || (item.map_type == 2 && !key.is_empty())
    {
        return Err(EncodeError::BadHashArrayCombo);
    }

    let mark = out.len();
    push_cstring4(out, item.name.as_str())?;
    out.push(item.map_type)
        .map_err(|_| EncodeError::BufferFull)?;
    push_u24le(out, item.len)?;
    push_u32le(out, item.map_vma)?;
    push_u32le(out, key.len() as u32)?;
    push_u32le(out, value.len() as u32)?;
    extend_meta(out, &key)?;
    extend_meta(out, &value)?;
    let consumed = out.len() - mark;
    let pad = align4(consumed) - consumed;
    for _ in 0..pad {
        out.push(0).map_err(|_| EncodeError::BufferFull)?;
    }
    Ok(())
}

fn layout_bytes(
    layout: Option<&heapless::Vec<u8, MAX_LAYOUT>>,
    max_bytes: usize,
) -> Result<heapless::Vec<u8, MAX_LAYOUT>, EncodeError> {
    let Some(v) = layout else {
        return Ok(heapless::Vec::new());
    };
    if v.is_empty() || v.len() > MAX_LAYOUT {
        return Err(EncodeError::BadLayout);
    }
    let mut sum: u32 = 0;
    for &bits in v.iter() {
        if !(1..=255).contains(&bits) {
            return Err(EncodeError::BadLayout);
        }
        sum = sum.saturating_add(u32::from(bits));
    }
    if !sum.is_multiple_of(8) || (sum as usize) > max_bytes * 8 {
        return Err(EncodeError::BadLayout);
    }
    Ok(v.clone())
}

fn check_name(name: &str) -> Result<(), EncodeError> {
    let raw = name.as_bytes();
    if !(1..=MAX_NAME).contains(&raw.len()) || !raw.is_ascii() || raw.contains(&0) {
        return Err(EncodeError::BadName);
    }
    Ok(())
}

fn check_address(value: u32) -> Result<(), EncodeError> {
    if !(4..=0xFFFF_FFFC).contains(&value) || !value.is_multiple_of(4) {
        return Err(EncodeError::BadAddress);
    }
    Ok(())
}

fn push_cstring4(
    out: &mut heapless::Vec<u8, MAX_META_SIZE>,
    name: &str,
) -> Result<(), EncodeError> {
    let start = out.len();
    extend_meta(out, name.as_bytes())?;
    out.push(0).map_err(|_| EncodeError::BufferFull)?;
    let encoded = out.len() - start;
    let pad = align4(encoded) - encoded;
    for _ in 0..pad {
        out.push(0).map_err(|_| EncodeError::BufferFull)?;
    }
    Ok(())
}

fn push_ehdr(out: &mut heapless::Vec<u8, MAX_CONTAINER_SIZE>) -> Result<(), EncodeError> {
    // IDENT(16) + type=1 + machine=243 + version=1 + entry=0 + phoff=0
    // + shoff=52 + flags=1 + ehsize=52 + phentsize=0 + phnum=0
    // + shentsize=40 + shnum=4 + shstrndx=3
    extend(out, &IDENT)?;
    push_u16(out, 1)?;
    push_u16(out, 243)?;
    push_u32(out, 1)?;
    push_u32(out, 0)?;
    push_u32(out, 0)?;
    push_u32(out, ELF32_EHDR_SIZE as u32)?;
    push_u32(out, 1)?;
    push_u16(out, ELF32_EHDR_SIZE)?;
    push_u16(out, 0)?;
    push_u16(out, 0)?;
    push_u16(out, 40)?;
    push_u16(out, 4)?;
    push_u16(out, 3)?;
    Ok(())
}

fn push_shdr(
    out: &mut heapless::Vec<u8, MAX_CONTAINER_SIZE>,
    fields: [u32; 10],
) -> Result<(), EncodeError> {
    for f in fields {
        push_u32(out, f)?;
    }
    Ok(())
}

fn push_u16(out: &mut heapless::Vec<u8, MAX_CONTAINER_SIZE>, v: u16) -> Result<(), EncodeError> {
    extend(out, &v.to_le_bytes())
}

fn push_u32(out: &mut heapless::Vec<u8, MAX_CONTAINER_SIZE>, v: u32) -> Result<(), EncodeError> {
    extend(out, &v.to_le_bytes())
}

fn push_u16le(out: &mut heapless::Vec<u8, MAX_META_SIZE>, v: u16) -> Result<(), EncodeError> {
    extend_meta(out, &v.to_le_bytes())
}

fn push_u32le(out: &mut heapless::Vec<u8, MAX_META_SIZE>, v: u32) -> Result<(), EncodeError> {
    extend_meta(out, &v.to_le_bytes())
}

fn push_u24le(out: &mut heapless::Vec<u8, MAX_META_SIZE>, v: u32) -> Result<(), EncodeError> {
    if v > 0xFF_FFFF {
        return Err(EncodeError::BadMapLen);
    }
    out.push((v & 0xFF) as u8)
        .map_err(|_| EncodeError::BufferFull)?;
    out.push(((v >> 8) & 0xFF) as u8)
        .map_err(|_| EncodeError::BufferFull)?;
    out.push(((v >> 16) & 0xFF) as u8)
        .map_err(|_| EncodeError::BufferFull)?;
    Ok(())
}

fn extend(
    out: &mut heapless::Vec<u8, MAX_CONTAINER_SIZE>,
    bytes: &[u8],
) -> Result<(), EncodeError> {
    out.extend_from_slice(bytes)
        .map_err(|_| EncodeError::BufferFull)
}

fn extend_meta(
    out: &mut heapless::Vec<u8, MAX_META_SIZE>,
    bytes: &[u8],
) -> Result<(), EncodeError> {
    out.extend_from_slice(bytes)
        .map_err(|_| EncodeError::BufferFull)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScanOutcome, scan_at};

    fn name(s: &str) -> heapless::String<{ MAX_NAME + 1 }> {
        heapless::String::try_from(s).unwrap()
    }

    fn layout(bits: &[u8]) -> heapless::Vec<u8, MAX_LAYOUT> {
        let mut v = heapless::Vec::new();
        v.extend_from_slice(bits).unwrap();
        v
    }

    fn example_meta() -> ImageMetadataV0 {
        let mut maps = heapless::Vec::new();
        maps.push(MapDescriptorV0 {
            name: name("stats"),
            map_type: 2,
            len: 16,
            map_vma: 0x4038_0100,
            key_layout: None,
            value_layout: Some(layout(&[32, 32])),
        })
        .unwrap();
        ImageMetadataV0 {
            slot_address: 0x4038_0000,
            helper_address: 0x4038_1000,
            machine: 1,
            name: name("example"),
            maps,
        }
    }

    #[test]
    fn golden_wire_vector() {
        let image = b"\x01\x00\x82\x80\xff";
        let meta = example_meta();
        let data = build_container(image, &meta, None).unwrap();

        let expected_meta = bytes_from_hex(
            "00 01 01 00 00 10 38 40 \
             65 78 61 6d 70 6c 65 00 \
             73 74 61 74 73 00 00 00 \
             02 10 00 00 00 01 38 40 00 00 00 00 02 00 00 00 \
             20 20 00 00",
        );
        assert_eq!(expected_meta.len(), 44);
        assert_eq!(&data[212..256], expected_meta.as_slice());
        assert_eq!(&data[256..261], image);
        assert_eq!(&data[261..297], SECTION_NAMES);
        assert_eq!(&data[297..300], &[0, 0, 0]);
        assert_eq!(data.len(), 332);
        assert_eq!(&data[data.len() - 32..], Sha256::digest(image).as_slice());
    }

    fn bytes_from_hex(s: &str) -> heapless::Vec<u8, 64> {
        let mut out = heapless::Vec::new();
        let mut nibble: Option<u8> = None;
        for b in s.bytes() {
            let v = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                b' ' | b'\n' | b'\t' => continue,
                _ => panic!("bad hex"),
            };
            match nibble {
                None => nibble = Some(v),
                Some(hi) => {
                    out.push((hi << 4) | v).unwrap();
                    nibble = None;
                }
            }
        }
        assert!(nibble.is_none());
        out
    }

    #[test]
    fn encode_scan_round_trip() {
        let image = b"\x01\x00\x82\x80\xff";
        let meta = example_meta();
        let data = build_container(image, &meta, None).unwrap();
        let mut src = data.as_slice();
        match scan_at(&mut src, 0) {
            ScanOutcome::Ok(c) => {
                assert_eq!(c.name.as_str(), "example");
                assert_eq!(c.slot_address, 0x4038_0000);
                assert_eq!(c.helper_address, 0x4038_1000);
                assert_eq!(c.image_size, image.len());
                assert_eq!(c.maps.len(), 1);
                assert_eq!(c.maps[0].name.as_str(), "stats");
                assert_eq!(c.maps[0].map_type, 2);
            }
            other => panic!("scan failed: {other:?}"),
        }
    }

    #[test]
    fn empty_image_rejected() {
        let meta = example_meta();
        assert_eq!(
            build_container(b"", &meta, None),
            Err(EncodeError::EmptyImage)
        );
    }

    #[test]
    fn slot_len_enforced() {
        let meta = example_meta();
        let image = b"\x01\x00\x82\x80\xff";
        assert_eq!(
            build_container(image, &meta, Some(4)),
            Err(EncodeError::ImageExceedsSlot)
        );
        assert!(build_container(image, &meta, Some(5)).is_ok());
    }

    #[test]
    fn exact_container_limit() {
        let mut meta = example_meta();
        meta.maps.clear();
        meta.name = name("x");
        let data = build_container(&[0u8; 3804], &meta, None).unwrap();
        assert_eq!(data.len(), 4096);
        assert_eq!(
            build_container(&[0u8; 3805], &meta, None),
            Err(EncodeError::ContainerTooLarge)
        );
    }
}
