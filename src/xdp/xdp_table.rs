//! Private cold-image storage owned exclusively by `XdpRt`.

use super::XdpTableId;
use anyhow::{Result, bail};

pub(super) struct XdpTable {
    bytes: &'static mut [u8],
    image_capacity: usize,
}

impl XdpTable {
    pub(super) fn new(bytes: &'static mut [u8], image_capacity: usize) -> Result<Self> {
        if image_capacity == 0 || bytes.is_empty() || bytes.len() % image_capacity != 0 {
            bail!("XDP table must contain a whole number of non-empty image entries");
        }
        Ok(Self {
            bytes,
            image_capacity,
        })
    }
    fn range(&self, id: XdpTableId) -> Result<core::ops::Range<usize>> {
        let start = id
            .checked_mul(self.image_capacity)
            .ok_or_else(|| anyhow::anyhow!("XDP table id overflow"))?;
        let end = start
            .checked_add(self.image_capacity)
            .ok_or_else(|| anyhow::anyhow!("XDP table id overflow"))?;
        if end > self.bytes.len() {
            bail!("XDP table id {id} is outside the table");
        }
        Ok(start..end)
    }
    pub(super) fn load(
        &self,
        id: XdpTableId,
        dst: *mut u8,
        len: usize,
        capacity: usize,
    ) -> Result<()> {
        if len == 0 || len > capacity || len > self.image_capacity {
            bail!("XDP image does not fit its target slot");
        }
        let range = self.range(id)?;
        unsafe {
            core::ptr::copy_nonoverlapping(self.bytes[range].as_ptr(), dst, len);
            core::ptr::write_bytes(dst.add(len), 0, capacity - len);
        }
        Ok(())
    }
    pub(super) fn save(&mut self, id: XdpTableId, src: *const u8, len: usize) -> Result<()> {
        if len == 0 || len > self.image_capacity {
            bail!("XDP image does not fit its table entry");
        }
        let range = self.range(id)?;
        unsafe {
            core::ptr::copy_nonoverlapping(src, self.bytes[range.clone()].as_mut_ptr(), len);
            core::ptr::write_bytes(
                self.bytes.as_mut_ptr().add(range.start + len),
                0,
                self.image_capacity - len,
            );
        }
        Ok(())
    }
}
