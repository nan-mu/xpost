//! Cold-image table backends owned exclusively by `XdpRt`.

use crate::error::FlashError;

/// Board-agnostic cold storage for XDP images.
///
/// `Send` is required because `XdpRt` lives in an embassy `Mutex`. On the
/// single-core MCU path the bound is a type-system requirement only.
pub(crate) trait XdpTableStorage: Send {
    fn capacity(&self) -> usize;
    fn image_capacity(&self) -> usize;
    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), FlashError>;
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
    ) -> Result<Self, FlashError> {
        use embedded_storage::nor_flash::ReadNorFlash;

        if image_capacity == 0 {
            return Err(FlashError::Storage);
        }
        if partition_len == 0
            || !partition_offset.is_multiple_of(esp_storage::FlashStorage::SECTOR_SIZE)
        {
            return Err(FlashError::Storage);
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

    fn abs(&self, offset: usize) -> Result<u32, FlashError> {
        let off = u32::try_from(offset).map_err(|_| FlashError::Storage)?;
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

    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), FlashError> {
        use embedded_storage::nor_flash::ReadNorFlash;
        if out.is_empty() {
            return Ok(());
        }
        if offset.saturating_add(out.len()) > self.partition_len as usize {
            return Err(FlashError::Storage);
        }
        // esp-storage without `bytewise-read` requires WORD_SIZE alignment on
        // offset and length (`NotAligned` → Storage). Compact ELF image payloads
        // need not be 4-byte sized; pad the physical read like xpost_elf::visit_bytes.
        #[repr(C, align(4))]
        struct Block([u8; 68]);
        let mut block = Block([0; 68]);
        let end = offset + out.len();
        let mut current = offset;
        let mut written = 0usize;
        while current < end {
            let n = (end - current).min(64);
            let prefix = current & 3;
            let physical_start = current & !3;
            let physical_len = (prefix + n + 3) & !3;
            if physical_start.saturating_add(physical_len) > self.partition_len as usize {
                return Err(FlashError::Storage);
            }
            self.flash
                .read(self.abs(physical_start)?, &mut block.0[..physical_len])
                .map_err(|_| FlashError::Storage)?;
            out[written..written + n].copy_from_slice(&block.0[prefix..prefix + n]);
            current += n;
            written += n;
        }
        Ok(())
    }
}
