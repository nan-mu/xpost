//! Firmware memory-map contract blob (`XPM1`) for host tools.
//!
//! Encoded into ELF section [SECTION_NAME] (non-ALLOC). This module is pure
//! bytes — reading symbols / writing the section is [xpost-build]'s job.

/// ELF section name for the mem-map contract (host-only, not `SHF_ALLOC`).
pub const SECTION_NAME: &str = ".xpost.mem";

/// Wire magic `XPM1` as little-endian `u32`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"XPM1");

/// Fixed payload size in bytes.
pub const WIRE_SIZE: usize = 24;

/// Single-slot firmware memory map (addresses are absolute MCU VAs / flash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemMapV0 {
    pub helper_addr: u32,
    pub slot_addr: u32,
    pub slot_len: u32,
    pub table_addr: u32,
    pub table_len: u32,
}

/// Decode / validate failures for [decode].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemMapError {
    Truncated,
    BadMagic,
    ZeroHelper,
    ZeroSlot,
    ZeroSlotLen,
    ZeroTableLen,
}

impl MemMapV0 {
    /// Encode to the fixed 24-byte LE wire image.
    pub fn encode(&self) -> [u8; WIRE_SIZE] {
        encode(self)
    }

    /// Decode and fail-closed on truncated / bad magic / zero critical fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, MemMapError> {
        decode(bytes)
    }
}

/// Encode to the fixed 24-byte LE wire image.
pub fn encode(map: &MemMapV0) -> [u8; WIRE_SIZE] {
    let mut out = [0u8; WIRE_SIZE];
    out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    out[4..8].copy_from_slice(&map.helper_addr.to_le_bytes());
    out[8..12].copy_from_slice(&map.slot_addr.to_le_bytes());
    out[12..16].copy_from_slice(&map.slot_len.to_le_bytes());
    out[16..20].copy_from_slice(&map.table_addr.to_le_bytes());
    out[20..24].copy_from_slice(&map.table_len.to_le_bytes());
    out
}

/// Decode and fail-closed on truncated / bad magic / zero critical fields.
pub fn decode(bytes: &[u8]) -> Result<MemMapV0, MemMapError> {
    if bytes.len() < WIRE_SIZE {
        return Err(MemMapError::Truncated);
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != MAGIC {
        return Err(MemMapError::BadMagic);
    }
    let helper_addr = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let slot_addr = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let slot_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let table_addr = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
    let table_len = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    if helper_addr == 0 {
        return Err(MemMapError::ZeroHelper);
    }
    if slot_addr == 0 {
        return Err(MemMapError::ZeroSlot);
    }
    if slot_len == 0 {
        return Err(MemMapError::ZeroSlotLen);
    }
    if table_len == 0 {
        return Err(MemMapError::ZeroTableLen);
    }
    Ok(MemMapV0 {
        helper_addr,
        slot_addr,
        slot_len,
        table_addr,
        table_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let m = MemMapV0 {
            helper_addr: 0x4038_0000,
            slot_addr: 0x4038_0200,
            slot_len: 0xE00,
            table_addr: 0x31_0000,
            table_len: 0x1_0000,
        };
        let wire = m.encode();
        assert_eq!(wire.len(), WIRE_SIZE);
        assert_eq!(MemMapV0::decode(&wire).unwrap(), m);
    }

    #[test]
    fn reject_bad_magic() {
        let mut wire = MemMapV0 {
            helper_addr: 1,
            slot_addr: 2,
            slot_len: 3,
            table_addr: 4,
            table_len: 5,
        }
        .encode();
        wire[0] = b'X';
        wire[1] = b'P';
        wire[2] = b'L';
        wire[3] = b'1';
        assert_eq!(MemMapV0::decode(&wire), Err(MemMapError::BadMagic));
    }
}
