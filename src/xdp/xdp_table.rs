//! Cold, mutable storage for XDP images.
//!
//! A board owns the static `XdpTable` declaration and therefore its linker
//! placement. `XdpRt` owns all access after initialization; callers move an
//! image through its [`super::XdpMeta`] rather than indexing this container.

use core::cell::UnsafeCell;

/// Compile-time identity of an image's cold-table position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct XdpTableId(pub(crate) usize);

impl XdpTableId {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }
    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpTableError {
    InvalidImage,
    EmptyImage,
    ImageTooLarge,
}

/// Fixed-capacity backing store for cold images.
///
/// The dimensions and placement are compile-time board choices. Each entry is
/// mutable so a program can be moved back from an executable slot.
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
            bytes: self.bytes.get().cast(),
            lengths: self.lengths.get().cast(),
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

#[derive(Clone, Copy)]
pub(crate) struct XdpTableHandle {
    bytes: *mut u8,
    lengths: *mut usize,
    image_count: usize,
    image_capacity: usize,
}
unsafe impl Send for XdpTableHandle {}

impl XdpTableHandle {
    fn range(self, id: XdpTableId) -> Result<(*mut u8, usize), XdpTableError> {
        if id.index() >= self.image_count {
            return Err(XdpTableError::InvalidImage);
        }
        let offset = id
            .index()
            .checked_mul(self.image_capacity)
            .ok_or(XdpTableError::InvalidImage)?;
        Ok((unsafe { self.bytes.add(offset) }, self.image_capacity))
    }
    pub(crate) fn copy_out(
        self,
        id: XdpTableId,
        dst: *mut u8,
        dst_capacity: usize,
    ) -> Result<usize, XdpTableError> {
        let (src, _) = self.range(id)?;
        let len = unsafe { *self.lengths.add(id.index()) };
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
    pub(crate) fn copy_in(
        self,
        id: XdpTableId,
        src: *const u8,
        len: usize,
    ) -> Result<(), XdpTableError> {
        let (dst, capacity) = self.range(id)?;
        if len == 0 {
            return Err(XdpTableError::EmptyImage);
        }
        if len > capacity {
            return Err(XdpTableError::ImageTooLarge);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(src, dst, len);
            core::ptr::write_bytes(dst.add(len), 0, capacity - len);
            *self.lengths.add(id.index()) = len;
        }
        Ok(())
    }
}
