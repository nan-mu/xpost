//! Cold-image table backends owned exclusively by `XdpRt`.

use crate::xdp::XdpError;

/// Board-agnostic cold storage for XDP images.
///
/// `Send` is required because `XdpRt` lives in an embassy `Mutex`. On the
/// single-core MCU path the bound is a type-system requirement only.
pub(crate) trait XdpTableStorage: Send {
    fn capacity(&self) -> usize;
    fn image_capacity(&self) -> usize;
    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), XdpError>;
    fn erase(&mut self, offset: usize, len: usize) -> Result<(), XdpError>;
    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), XdpError>;
}

/// Default image stride when embedder hands a flat SRAM/DRAM blob.
pub(crate) const DEFAULT_IMAGE_CAPACITY: usize = 4096;

/// Packed Compact ELF region in Flash (`espflash write-bin` target).
#[derive(Clone, Copy, Debug)]
pub struct ColdRegion {
    /// Absolute chip flash address of the region start.
    pub flash_base: u32,
    /// Region length in bytes.
    pub flash_len: u32,
    /// Max size of one Compact ELF container.
    pub image_capacity: usize,
}

impl ColdRegion {
    pub const fn new(flash_base: usize, flash_len: usize, image_capacity: usize) -> Self {
        Self {
            flash_base: flash_base as u32,
            flash_len: flash_len as u32,
            image_capacity,
        }
    }
}

impl From<(usize, usize, usize)> for ColdRegion {
    /// `(flash_base, flash_len, image_capacity)` — same order as codegen `TABLE`.
    fn from((flash_base, flash_len, image_capacity): (usize, usize, usize)) -> Self {
        Self::new(flash_base, flash_len, image_capacity)
    }
}

/// Flash-backed cold table. [`crate::xdp::XdpRt::init`] constructs this from
/// layout `[cold_table]` / codegen `TABLE`.
pub(crate) struct EspFlashXdpTable {
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
        // `image_capacity` is max single Compact ELF size, not a cell stride.
        let _ = flash.capacity();
        Ok(Self {
            flash,
            partition_offset,
            partition_len,
            image_capacity,
        })
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
