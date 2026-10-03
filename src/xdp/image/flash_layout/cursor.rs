//! Sequential parser over an SRAM metadata slice (no Flash I/O).

use super::ReadError;

const MAX_NAME: usize = 31;

#[inline(always)]
fn align4(x: usize) -> usize {
    (x + 3) & !3
}

/// Cursor that only shortens `rest` over a borrowed metadata buffer.
pub(crate) struct MetaCursor<'a> {
    rest: &'a [u8],
}

impl<'a> MetaCursor<'a> {
    #[inline(always)]
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    #[inline(always)]
    pub(crate) fn remaining(&self) -> usize {
        self.rest.len()
    }

    #[inline]
    pub(crate) fn take_bytes(&mut self, len: usize) -> Result<&'a [u8], ReadError> {
        if self.rest.len() < len {
            return Err(ReadError::Truncated);
        }
        let (value, rest) = self.rest.split_at(len);
        self.rest = rest;
        Ok(value)
    }

    #[inline]
    pub(crate) fn take_u8(&mut self) -> Result<u8, ReadError> {
        Ok(self.take_bytes(1)?[0])
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn take_u16_le(&mut self) -> Result<u16, ReadError> {
        let b = self.take_bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    #[inline]
    pub(crate) fn take_u24_le(&mut self) -> Result<u32, ReadError> {
        let b = self.take_bytes(3)?;
        Ok((b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16))
    }

    #[inline]
    pub(crate) fn take_u32_le(&mut self) -> Result<u32, ReadError> {
        let b = self.take_bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// CString4: 1..31 ASCII + NUL + zero pad to 4-byte alignment. Returns name without NUL.
    pub(crate) fn take_cstring4(&mut self) -> Result<&'a str, ReadError> {
        let nul = self
            .rest
            .iter()
            .position(|&c| c == 0)
            .ok_or(ReadError::Truncated)?;
        if nul == 0 || nul > MAX_NAME {
            return Err(ReadError::Truncated);
        }
        let encoded = align4(nul + 1);
        if self.rest.len() < encoded {
            return Err(ReadError::Truncated);
        }
        let raw = &self.rest[..nul];
        if !raw.is_ascii() {
            return Err(ReadError::Truncated);
        }
        if self.rest[nul + 1..encoded].iter().any(|&c| c != 0) {
            return Err(ReadError::Truncated);
        }
        let name = core::str::from_utf8(raw).map_err(|_| ReadError::Truncated)?;
        self.rest = &self.rest[encoded..];
        Ok(name)
    }

    /// Consume 0..3 zero padding bytes so the absolute consumed length is 4-aligned
    /// relative to the start of this cursor's original slice is not tracked —
    /// pad so `taken % 4 == 0` from the record start by consuming until
    /// `align4(consumed_in_record)`.
    ///
    /// Wire rule: after a map record body, pad with zeros to the next 4-byte
    /// boundary counted from the **start of the metadata section**. Callers
    /// that need absolute alignment should use [`Self::align4_from`] with the
    /// absolute offset into the section, or pad relative to bytes already
    /// consumed from a known-aligned base.
    ///
    /// Here we pad so the number of bytes removed from this cursor since the
    /// last align point equals: consume zeros until `remaining` length shares
    /// the same mod-4 as needed. Practical form used by MapDescriptor: after
    /// layouts, pad with zeros until `(start_len - remaining) % 4 == 0` where
    /// `start_len` is captured at record start.
    pub(crate) fn align4_consumed(&mut self, consumed: usize) -> Result<(), ReadError> {
        let pad = align4(consumed) - consumed;
        if pad == 0 {
            return Ok(());
        }
        let bytes = self.take_bytes(pad)?;
        if bytes.iter().any(|&c| c != 0) {
            return Err(ReadError::Truncated);
        }
        Ok(())
    }
}
