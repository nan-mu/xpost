//! Cold-image table backends owned exclusively by `XdpRt`.

use crate::xdp::XdpError;

/// Board-agnostic cold storage for XDP images.
///
/// `Send` is required because `XdpRt` lives in an embassy `Mutex`. On the
/// single-core MCU path the bound is a type-system requirement only.
pub trait XdpTableStorage: Send {
    fn capacity(&self) -> usize;
    fn image_capacity(&self) -> usize;
    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), XdpError>;
    fn erase(&mut self, offset: usize, len: usize) -> Result<(), XdpError>;
    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), XdpError>;
}

/// Default image stride when embedder hands a flat SRAM/DRAM blob.
pub const DEFAULT_IMAGE_CAPACITY: usize = 4096;

/// SRAM fallback for [`crate::xdp::XdpRt::init`] when no Flash table is claimed.
///
/// Sized for ~two default images — ESP32-C3 has ~400 KiB SRAM total; a 512 KiB
/// BSS table is not viable. Prefer [`crate::xdp::XdpRt::claim_table`] / Flash
/// `[cold_table]` for real capacity.
pub(crate) const COLD_TABLE_BYTES: usize = 2 * DEFAULT_IMAGE_CAPACITY;

/// Internal cold-image BSS used until [`crate::xdp::XdpRt::claim_table`].
pub(crate) static mut COLD_TABLE: [u8; COLD_TABLE_BYTES] = [0u8; COLD_TABLE_BYTES];

/// Embedder-owned XDP table buffer produced by `declare_runtime!`.
///
/// Pass `&mut XDP_TABLE` into [`crate::xdp::XdpRt::init`].
pub struct XdpTable<const N: usize> {
    bytes: [u8; N],
    image_capacity: usize,
}

impl<const N: usize> XdpTable<N> {
    pub const fn new(image_capacity: usize) -> Self {
        Self {
            bytes: [0u8; N],
            image_capacity,
        }
    }

    pub fn image_capacity(&self) -> usize {
        self.image_capacity
    }

    pub fn as_bytes_mut(&'static mut self) -> &'static mut [u8] {
        &mut self.bytes
    }
}

pub(crate) struct MemoryXdpTable {
    bytes: &'static mut [u8],
    image_capacity: usize,
}

impl MemoryXdpTable {
    pub(crate) fn try_new(storage: &'static mut [u8]) -> Result<Self, XdpError> {
        Self::try_with_image_capacity(storage, DEFAULT_IMAGE_CAPACITY)
    }

    pub(crate) fn try_with_image_capacity(
        storage: &'static mut [u8],
        image_capacity: usize,
    ) -> Result<Self, XdpError> {
        if image_capacity == 0 || storage.is_empty() || storage.len() % image_capacity != 0 {
            return Err(XdpError::Storage);
        }
        Ok(Self {
            bytes: storage,
            image_capacity,
        })
    }
}

impl XdpTableStorage for MemoryXdpTable {
    fn capacity(&self) -> usize {
        self.bytes.len()
    }

    fn image_capacity(&self) -> usize {
        self.image_capacity
    }

    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), XdpError> {
        let end = offset.checked_add(out.len()).ok_or(XdpError::Storage)?;
        if end > self.bytes.len() {
            return Err(XdpError::Storage);
        }
        out.copy_from_slice(&self.bytes[offset..end]);
        Ok(())
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), XdpError> {
        let end = offset.checked_add(len).ok_or(XdpError::Storage)?;
        if end > self.bytes.len() {
            return Err(XdpError::Storage);
        }
        self.bytes[offset..end].fill(0);
        Ok(())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), XdpError> {
        let end = offset.checked_add(bytes.len()).ok_or(XdpError::Storage)?;
        if end > self.bytes.len() {
            return Err(XdpError::Storage);
        }
        self.bytes[offset..end].copy_from_slice(bytes);
        Ok(())
    }
}

/// Flash-backed cold table. Wire via [`crate::xdp::XdpRt::claim_table`] after
/// constructing with partition offset/len from layout codegen
/// (`COLD_TABLE_FLASH_*`).
pub struct EspFlashXdpTable {
    flash: esp_storage::FlashStorage<'static>,
    partition_offset: u32,
    partition_len: u32,
    image_capacity: usize,
}

impl EspFlashXdpTable {
    /// `partition_offset` / `partition_len` are flash-chip absolute addresses
    /// matching layout `[cold_table]` / `espflash write-bin`.
    pub fn try_new(
        flash: esp_storage::FlashStorage<'static>,
        partition_offset: u32,
        partition_len: u32,
        image_capacity: usize,
    ) -> Result<Self, XdpError> {
        use embedded_storage::nor_flash::ReadNorFlash;

        if image_capacity == 0 {
            return Err(XdpError::Storage);
        }
        if partition_len == 0 || partition_offset % esp_storage::FlashStorage::SECTOR_SIZE != 0 {
            return Err(XdpError::Storage);
        }
        if partition_len as usize % image_capacity != 0 {
            return Err(XdpError::Storage);
        }
        let _ = flash.capacity();
        Ok(Self {
            flash,
            partition_offset,
            partition_len,
            image_capacity,
        })
    }

    /// Absolute flash address of cold-table cell `table_id` (for host write-bin).
    pub fn image_abs(&self, table_id: usize) -> Result<u32, XdpError> {
        let off = table_id
            .checked_mul(self.image_capacity)
            .ok_or(XdpError::Storage)?;
        if off.saturating_add(self.image_capacity) > self.partition_len as usize {
            return Err(XdpError::Storage);
        }
        let off_u = u32::try_from(off).map_err(|_| XdpError::Storage)?;
        Ok(self.partition_offset.saturating_add(off_u))
    }

    fn abs(&self, offset: usize) -> Result<u32, XdpError> {
        let off = u32::try_from(offset).map_err(|_| XdpError::Storage)?;
        Ok(self.partition_offset.saturating_add(off))
    }
}

impl XdpTableStorage for EspFlashXdpTable {
    fn capacity(&self) -> usize {
        self.partition_len as usize
    }

    fn image_capacity(&self) -> usize {
        self.image_capacity
    }

    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), XdpError> {
        use embedded_storage::nor_flash::ReadNorFlash;
        if offset.saturating_add(out.len()) > self.partition_len as usize {
            return Err(XdpError::Storage);
        }
        self.flash
            .read(self.abs(offset)?, out)
            .map_err(|_| XdpError::Storage)
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), XdpError> {
        use embedded_storage::nor_flash::NorFlash;
        if offset.saturating_add(len) > self.partition_len as usize {
            return Err(XdpError::Storage);
        }
        let start = self.abs(offset)?;
        let end = start.saturating_add(len as u32);
        self.flash.erase(start, end).map_err(|_| XdpError::Storage)
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), XdpError> {
        use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
        if offset.saturating_add(bytes.len()) > self.partition_len as usize {
            return Err(XdpError::Storage);
        }
        if (bytes.as_ptr() as usize) % 4 != 0 || bytes.len() % 4 != 0 {
            return Err(XdpError::Storage);
        }
        let abs = self.abs(offset)?;
        self.flash
            .write(abs, bytes)
            .map_err(|_| XdpError::Storage)?;
        let mut verify = [0u8; 64];
        let mut done = 0usize;
        while done < bytes.len() {
            let n = (bytes.len() - done).min(verify.len());
            self.flash
                .read(abs + done as u32, &mut verify[..n])
                .map_err(|_| XdpError::Storage)?;
            if verify[..n] != bytes[done..done + n] {
                return Err(XdpError::Storage);
            }
            done += n;
        }
        Ok(())
    }
}
