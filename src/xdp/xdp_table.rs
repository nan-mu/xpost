//! Statically sized, mutable cold-image storage.
//!
//! Firmware chooses the table's capacity and linker section when declaring a
//! static `XdpTable`. This type does not allocate and does not decide where
//! the bytes live; a board linker script may place it in a writable Flash
//! window, RAM, or another non-executable cold region.

use core::cell::UnsafeCell;

/// Errors returned by XDP image-table operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpTableError {
    InvalidImage,
    EmptyImage,
    ImageTooLarge,
}

/// Fixed-capacity, mutable storage for cold XDP images.
///
/// `IMAGES` and `IMAGE_CAPACITY` are compile-time choices. A board supplies
/// placement, for example with `#[link_section = ".xdp.flash"]` on its
/// static instance. Every entry carries an explicit loaded length, so a zeroed
/// entry is not executable.
#[repr(align(4))]
pub struct XdpTable<const IMAGES: usize, const IMAGE_CAPACITY: usize> {
    bytes: UnsafeCell<[[u8; IMAGE_CAPACITY]; IMAGES]>,
    lengths: UnsafeCell<[usize; IMAGES]>,
}

unsafe impl<const IMAGES: usize, const IMAGE_CAPACITY: usize> Sync
    for XdpTable<IMAGES, IMAGE_CAPACITY>
{
}

impl<const IMAGES: usize, const IMAGE_CAPACITY: usize> XdpTable<IMAGES, IMAGE_CAPACITY> {
    pub const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new([[0; IMAGE_CAPACITY]; IMAGES]),
            lengths: UnsafeCell::new([0; IMAGES]),
        }
    }

    pub const fn image_count(&self) -> usize {
        IMAGES
    }

    pub const fn image_capacity(&self) -> usize {
        IMAGE_CAPACITY
    }

    pub(crate) fn handle(&'static self) -> XdpTableHandle {
        XdpTableHandle {
            bytes: self.bytes.get().cast::<u8>(),
            lengths: self.lengths.get().cast::<usize>(),
            image_count: IMAGES,
            image_capacity: IMAGE_CAPACITY,
        }
    }
}

impl<const IMAGES: usize, const IMAGE_CAPACITY: usize> Default
    for XdpTable<IMAGES, IMAGE_CAPACITY>
{
    fn default() -> Self {
        Self::new()
    }
}

/// Erased handle retained by [`super::XdpRt`] after initialization.
///
/// It keeps the static container private while allowing the runtime to stay
/// independent of a board's table dimensions.
#[derive(Clone, Copy)]
pub(crate) struct XdpTableHandle {
    bytes: *mut u8,
    lengths: *mut usize,
    image_count: usize,
    image_capacity: usize,
}

unsafe impl Send for XdpTableHandle {}

impl XdpTableHandle {
    fn range(&self, image: usize) -> Result<(*mut u8, usize), XdpTableError> {
        if image >= self.image_count {
            return Err(XdpTableError::InvalidImage);
        }
        let offset = image
            .checked_mul(self.image_capacity)
            .ok_or(XdpTableError::InvalidImage)?;
        let ptr = unsafe { self.bytes.add(offset) };
        Ok((ptr, self.image_capacity))
    }

    pub(crate) fn copy_to_slot(
        &self,
        image: usize,
        dst: *mut u8,
        dst_capacity: usize,
    ) -> Result<usize, XdpTableError> {
        let (src, _) = self.range(image)?;
        let len = unsafe { *self.lengths.add(image) };
        if len == 0 {
            return Err(XdpTableError::EmptyImage);
        }
        if len > dst_capacity {
            return Err(XdpTableError::ImageTooLarge);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, len);
            core::ptr::write_bytes(dst.add(len), 0, dst_capacity - len);
        }
        Ok(len)
    }

    pub(crate) fn copy_from_slot(
        &self,
        image: usize,
        src: *const u8,
        len: usize,
    ) -> Result<(), XdpTableError> {
        let (dst, capacity) = self.range(image)?;
        if len == 0 {
            return Err(XdpTableError::EmptyImage);
        }
        if len > capacity {
            return Err(XdpTableError::ImageTooLarge);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, len);
            core::ptr::write_bytes(dst.add(len), 0, capacity - len);
            *self.lengths.add(image) = len;
        }
        Ok(())
    }

    /// Exchange one table image with an executable slot without allocation.
    pub(crate) fn swap_with_slot(
        &self,
        image: usize,
        slot: *mut u8,
        slot_capacity: usize,
        slot_len: usize,
    ) -> Result<usize, XdpTableError> {
        let (table, table_capacity) = self.range(image)?;
        let table_len = unsafe { *self.lengths.add(image) };
        if table_len == 0 || slot_len == 0 {
            return Err(XdpTableError::EmptyImage);
        }
        if table_len > slot_capacity || slot_len > table_capacity {
            return Err(XdpTableError::ImageTooLarge);
        }
        let span = table_len.max(slot_len);
        for offset in 0..span {
            unsafe {
                let old_slot = *slot.add(offset);
                *slot.add(offset) = *table.add(offset);
                *table.add(offset) = old_slot;
            }
        }
        unsafe {
            *self.lengths.add(image) = slot_len;
        }
        Ok(table_len)
    }
}
