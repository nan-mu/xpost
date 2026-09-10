//! Cold-image table backends owned exclusively by `XdpRt`.

/// Board-agnostic cold storage for XDP images.
///
/// `Send` is required because `XdpRt` lives in an embassy `Mutex`. On the
/// single-core MCU path the bound is a type-system requirement only.
pub trait XdpTableStorage: Send {
    fn capacity(&self) -> usize;
    fn image_capacity(&self) -> usize;
    fn read(&mut self, offset: usize, out: &mut [u8]);
    fn erase(&mut self, offset: usize, len: usize);
    fn write(&mut self, offset: usize, bytes: &[u8]);
}

/// Default image stride when embedder hands a flat SRAM/DRAM blob.
pub const DEFAULT_IMAGE_CAPACITY: usize = 4096;

/// Cold-image table capacity built inside [`crate::xdp::XdpRt::init`].
///
/// TODO: expose capacity / flash backend to the board without a public table trait.
pub const COLD_TABLE_BYTES: usize = 512 * 1024;

pub struct MemoryXdpTable {
    bytes: &'static mut [u8],
    image_capacity: usize,
}

impl MemoryXdpTable {
    pub fn try_new(storage: &'static mut [u8]) -> Result<Self, crate::xdp::XdpError> {
        Self::try_with_image_capacity(storage, DEFAULT_IMAGE_CAPACITY)
    }

    pub fn try_with_image_capacity(
        storage: &'static mut [u8],
        image_capacity: usize,
    ) -> Result<Self, crate::xdp::XdpError> {
        if image_capacity == 0 || storage.is_empty() || storage.len() % image_capacity != 0 {
            return Err(crate::xdp::XdpError::Storage);
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

    fn read(&mut self, offset: usize, out: &mut [u8]) {
        let end = offset
            .checked_add(out.len())
            .unwrap_or_else(|| unimplemented!("TODO: collect Xdp storage errors (read OOB)"));
        if end > self.bytes.len() {
            unimplemented!("TODO: collect Xdp storage errors (read OOB)");
        }
        out.copy_from_slice(&self.bytes[offset..end]);
    }

    fn erase(&mut self, offset: usize, len: usize) {
        let end = offset
            .checked_add(len)
            .unwrap_or_else(|| unimplemented!("TODO: collect Xdp storage errors (erase OOB)"));
        if end > self.bytes.len() {
            unimplemented!("TODO: collect Xdp storage errors (erase OOB)");
        }
        self.bytes[offset..end].fill(0);
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        let end = offset
            .checked_add(bytes.len())
            .unwrap_or_else(|| unimplemented!("TODO: collect Xdp storage errors (write OOB)"));
        if end > self.bytes.len() {
            unimplemented!("TODO: collect Xdp storage errors (write OOB)");
        }
        self.bytes[offset..end].copy_from_slice(bytes);
    }
}

#[cfg(feature = "esp32c3-storage")]
/// Flash-backed cold table (not wired to [`super::XdpRt::init`] yet).
///
/// TODO: expose capacity / backend selection without a public storage trait.
#[allow(dead_code)]
pub struct EspFlashXdpTable<'d> {
    flash: esp_storage::FlashStorage<'d>,
    partition_offset: u32,
    partition_len: u32,
    image_capacity: usize,
}

#[cfg(feature = "esp32c3-storage")]
#[allow(dead_code)]
impl<'d> EspFlashXdpTable<'d> {
    pub fn new(
        flash: esp_storage::FlashStorage<'d>,
        partition_offset: u32,
        partition_len: u32,
    ) -> Self {
        use embedded_storage::nor_flash::ReadNorFlash;

        if partition_len == 0 || partition_offset % esp_storage::FlashStorage::SECTOR_SIZE != 0 {
            unimplemented!("TODO: collect Xdp storage errors (flash partition)");
        }
        if partition_len as usize % DEFAULT_IMAGE_CAPACITY != 0 {
            unimplemented!("TODO: collect Xdp storage errors (flash image stride)");
        }
        let _ = flash.capacity();
        Self {
            flash,
            partition_offset,
            partition_len,
            image_capacity: DEFAULT_IMAGE_CAPACITY,
        }
    }

    fn abs(&self, offset: usize) -> u32 {
        let off = u32::try_from(offset)
            .unwrap_or_else(|_| unimplemented!("TODO: collect Xdp storage errors (offset)"));
        self.partition_offset.saturating_add(off)
    }
}

#[cfg(feature = "esp32c3-storage")]
#[allow(dead_code)]
impl XdpTableStorage for EspFlashXdpTable<'_> {
    fn capacity(&self) -> usize {
        self.partition_len as usize
    }

    fn image_capacity(&self) -> usize {
        self.image_capacity
    }

    fn read(&mut self, offset: usize, out: &mut [u8]) {
        use embedded_storage::nor_flash::ReadNorFlash;
        if offset.saturating_add(out.len()) > self.partition_len as usize {
            unimplemented!("TODO: collect Xdp storage errors (flash read OOB)");
        }
        if self.flash.read(self.abs(offset), out).is_err() {
            unimplemented!("TODO: collect Xdp storage errors (flash read)");
        }
    }

    fn erase(&mut self, offset: usize, len: usize) {
        use embedded_storage::nor_flash::NorFlash;
        if offset.saturating_add(len) > self.partition_len as usize {
            unimplemented!("TODO: collect Xdp storage errors (flash erase OOB)");
        }
        let start = self.abs(offset);
        let end = start.saturating_add(len as u32);
        if self.flash.erase(start, end).is_err() {
            unimplemented!("TODO: collect Xdp storage errors (flash erase)");
        }
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) {
        use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
        if offset.saturating_add(bytes.len()) > self.partition_len as usize {
            unimplemented!("TODO: collect Xdp storage errors (flash write OOB)");
        }
        if (bytes.as_ptr() as usize) % 4 != 0 || bytes.len() % 4 != 0 {
            unimplemented!("TODO: collect Xdp storage errors (flash write align)");
        }
        let abs = self.abs(offset);
        if self.flash.write(abs, bytes).is_err() {
            unimplemented!("TODO: collect Xdp storage errors (flash write)");
        }
        let mut verify = [0u8; 64];
        let mut done = 0usize;
        while done < bytes.len() {
            let n = (bytes.len() - done).min(verify.len());
            if self
                .flash
                .read(abs + done as u32, &mut verify[..n])
                .is_err()
            {
                unimplemented!("TODO: collect Xdp storage errors (flash verify read)");
            }
            if verify[..n] != bytes[done..done + n] {
                unimplemented!("TODO: collect Xdp storage errors (flash verify)");
            }
            done += n;
        }
    }
}
