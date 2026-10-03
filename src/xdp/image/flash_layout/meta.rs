//! `.xpost.meta` owner: one bulk SRAM buffer + prefix getters + map iterator.

use crate::xdp::{XdpTableStorage, state::MAX_REQUIRED_MAPS};

use super::{
    LayoutError, ReadError, XpostMetaPrefix,
    buffer::FlashBuffer,
    cursor::MetaCursor,
};

pub(crate) const MAX_META_SIZE: usize = 1024;
const MAX_LAYOUT: usize = 32;
const MAX_ENTRIES: u32 = 64;
const MAX_KEY_BYTES: usize = 64;
const MAX_VALUE_BYTES: usize = 256;

/// Zero-copy MapDescriptorV0 view into [`XpostMeta`] bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MapDescriptor<'a> {
    pub name: &'a str,
    pub map_type: u8,
    pub len: u32,
    pub map_vma: u32,
    pub key_layout: Option<&'a [u8]>,
    pub value_layout: Option<&'a [u8]>,
}

/// Owned `.xpost.meta` section bytes (capacity 1024).
pub(crate) struct XpostMeta {
    bytes: heapless::Vec<u8, MAX_META_SIZE>,
}

impl XpostMeta {
    pub const MAX_SIZE: usize = MAX_META_SIZE;

    /// One bulk read of the META section into SRAM. No further Flash I/O for parsing.
    pub(crate) fn read(
        table: &mut dyn XdpTableStorage,
        section_offset: usize,
        section_size: usize,
    ) -> Result<Self, ReadError> {
        if section_size > Self::MAX_SIZE {
            return Err(ReadError::MetadataTooLarge);
        }
        if section_size < XpostMetaPrefix::WIRE_SIZE || section_size % 4 != 0 {
            return Err(ReadError::Truncated);
        }
        let buf = FlashBuffer::<MAX_META_SIZE>::read(table, section_offset, section_size)?;
        Ok(Self {
            bytes: buf.into_inner(),
        })
    }

    #[inline(always)]
    #[allow(dead_code)]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    fn prefix(&self) -> Result<XpostMetaPrefix<'_>, LayoutError> {
        XpostMetaPrefix::parse(self.bytes.as_slice())
    }

    #[inline]
    pub(crate) fn xpost_version(&self) -> Result<u8, LayoutError> {
        Ok(self.prefix()?.xpost_version())
    }

    #[inline]
    pub(crate) fn machine(&self) -> Result<u8, LayoutError> {
        Ok(self.prefix()?.machine())
    }

    #[inline]
    pub(crate) fn map_count(&self) -> Result<u16, LayoutError> {
        Ok(self.prefix()?.map_count())
    }

    #[inline]
    pub(crate) fn helper_address(&self) -> Result<u32, LayoutError> {
        Ok(self.prefix()?.helper_address())
    }

    /// Cursor over bytes after the fixed 8-byte prefix.
    pub(crate) fn cursor(&self) -> MetaCursor<'_> {
        MetaCursor::new(&self.bytes[XpostMetaPrefix::WIRE_SIZE..])
    }

    /// Parse `xdp_name` then return a map iterator. Caller must drain maps and
    /// then ensure the iterator reports exhaustion (`finish`).
    pub(crate) fn xdp_name_and_maps(&self) -> Result<(&str, MapIter<'_>), ReadError> {
        let prefix = self.prefix().map_err(|_| ReadError::Truncated)?;
        let count = prefix.map_count();
        if count as usize > MAX_REQUIRED_MAPS {
            return Err(ReadError::Truncated);
        }
        let mut cursor = self.cursor();
        let name = cursor.take_cstring4()?;
        Ok((
            name,
            MapIter {
                cursor,
                remaining: count,
            },
        ))
    }
}

pub(crate) struct MapIter<'a> {
    cursor: MetaCursor<'a>,
    remaining: u16,
}

impl<'a> MapIter<'a> {
    /// After all maps are yielded, require `remaining == 0` on the cursor.
    pub(crate) fn finish(self) -> Result<(), ReadError> {
        if self.remaining != 0 || self.cursor.remaining() != 0 {
            return Err(ReadError::Truncated);
        }
        Ok(())
    }
}

impl<'a> Iterator for MapIter<'a> {
    type Item = Result<MapDescriptor<'a>, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        Some(parse_map(&mut self.cursor))
    }
}

fn validate_layout(layout: &[u8], max_bytes: usize) -> Result<(), ReadError> {
    if layout.is_empty() || layout.len() > MAX_LAYOUT {
        return Err(ReadError::Truncated);
    }
    let mut bits: usize = 0;
    for &b in layout {
        if b == 0 {
            return Err(ReadError::Truncated);
        }
        bits = bits.checked_add(b as usize).ok_or(ReadError::Truncated)?;
    }
    if bits % 8 != 0 || bits / 8 > max_bytes {
        return Err(ReadError::Truncated);
    }
    Ok(())
}

fn parse_map<'a>(cursor: &mut MetaCursor<'a>) -> Result<MapDescriptor<'a>, ReadError> {
    let mark = cursor.remaining();
    let name = cursor.take_cstring4()?;
    let map_type = cursor.take_u8()?;
    let len = cursor.take_u24_le()?;
    let map_vma = cursor.take_u32_le()?;
    let key_len = cursor.take_u32_le()? as usize;
    let value_len = cursor.take_u32_le()? as usize;
    if key_len > MAX_LAYOUT || value_len > MAX_LAYOUT {
        return Err(ReadError::Truncated);
    }
    let key_layout = if key_len == 0 {
        None
    } else {
        let k = cursor.take_bytes(key_len)?;
        validate_layout(k, MAX_KEY_BYTES)?;
        Some(k)
    };
    let value_layout = if value_len == 0 {
        None
    } else {
        let v = cursor.take_bytes(value_len)?;
        validate_layout(v, MAX_VALUE_BYTES)?;
        Some(v)
    };
    let consumed = mark - cursor.remaining();
    cursor.align4_consumed(consumed)?;

    if !(1..=MAX_ENTRIES).contains(&len) {
        return Err(ReadError::Truncated);
    }
    if map_vma == 0 || (map_vma & 3) != 0 {
        return Err(ReadError::Truncated);
    }
    match map_type {
        1 => {
            if key_layout.is_none() || value_layout.is_none() {
                return Err(ReadError::Truncated);
            }
        }
        2 => {
            if key_layout.is_some() || value_layout.is_none() {
                return Err(ReadError::Truncated);
            }
        }
        _ => return Err(ReadError::Truncated),
    }

    Ok(MapDescriptor {
        name,
        map_type,
        len,
        map_vma,
        key_layout,
        value_layout,
    })
}
