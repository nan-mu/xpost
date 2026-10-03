//! One-shot bulk read into a bounded SRAM buffer (`heapless::Vec`).

use crate::xdp::{XdpTableStorage, error::XdpError};

use super::ReadError;

/// Fixed-capacity Flash/table read buffer. `N` is capacity; `len()` is valid bytes.
pub(crate) struct FlashBuffer<const N: usize> {
    bytes: heapless::Vec<u8, N>,
}

impl<const N: usize> FlashBuffer<N> {
    /// Bulk-read `len` bytes at region `offset` via `resize_default` (no `set_len`).
    pub(crate) fn read(
        table: &mut dyn XdpTableStorage,
        offset: usize,
        len: usize,
    ) -> Result<Self, ReadError> {
        if len > N {
            return Err(ReadError::OutOfBounds);
        }
        let end = offset.checked_add(len).ok_or(ReadError::AddressOverflow)?;
        if end > table.capacity() {
            return Err(ReadError::OutOfBounds);
        }
        let mut bytes = heapless::Vec::<u8, N>::new();
        bytes
            .resize_default(len)
            .map_err(|_| ReadError::OutOfBounds)?;
        table
            .read(offset, bytes.as_mut())
            .map_err(|_| ReadError::Storage)?;
        Ok(Self { bytes })
    }

    #[inline(always)]
    pub(crate) fn as_slice(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    #[inline(always)]
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    #[inline(always)]
    pub(crate) fn into_inner(self) -> heapless::Vec<u8, N> {
        self.bytes
    }
}

impl From<ReadError> for XdpError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::Storage => XdpError::Storage,
            ReadError::AddressOverflow | ReadError::OutOfBounds | ReadError::Truncated => {
                XdpError::ImageMagic
            }
            ReadError::MetadataTooLarge => XdpError::ImageTooLarge,
        }
    }
}
