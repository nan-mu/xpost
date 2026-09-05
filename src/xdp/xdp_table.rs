//! Flash-resident XDP image catalogue.
//!
//! Images remain owned by board firmware, usually through `include_bytes!`.
//! The table only records their static address, length, and a diagnostic name;
//! loading copies a selected image into an executable RAM/IRAM slot.

/// Maximum Flash images that one XDP runtime can catalogue.
pub const MAX_IMAGES: usize = 8;

/// Stable handle returned when an image enters an [`XdpTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XdpImageId(u8);

impl XdpImageId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One immutable image stored in Flash.
#[derive(Clone, Copy)]
pub struct XdpImage {
    name: &'static str,
    bytes: &'static [u8],
}

impl XdpImage {
    pub const fn name(self) -> &'static str {
        self.name
    }

    pub const fn bytes(self) -> &'static [u8] {
        self.bytes
    }
}

/// Image-catalogue errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XdpTableError {
    EmptyImage,
    DuplicateImage,
    TableFull,
}

/// Bounded catalogue of immutable XDP images in Flash.
pub struct XdpTable {
    images: heapless::Vec<XdpImage, MAX_IMAGES>,
}

impl XdpTable {
    pub const fn new() -> Self {
        Self {
            images: heapless::Vec::new(),
        }
    }

    /// Register a Flash image and return an id that remains stable for the
    /// lifetime of this runtime.
    pub fn register(
        &mut self,
        name: &'static str,
        bytes: &'static [u8],
    ) -> Result<XdpImageId, XdpTableError> {
        if bytes.is_empty() {
            return Err(XdpTableError::EmptyImage);
        }
        if self.images.iter().any(|image| {
            image.name == name
                || (image.bytes.len() == bytes.len()
                    && core::ptr::eq(image.bytes.as_ptr(), bytes.as_ptr()))
        }) {
            return Err(XdpTableError::DuplicateImage);
        }
        let id = XdpImageId(self.images.len() as u8);
        self.images
            .push(XdpImage { name, bytes })
            .map_err(|_| XdpTableError::TableFull)?;
        Ok(id)
    }

    pub fn get(&self, id: XdpImageId) -> Option<XdpImage> {
        self.images.get(id.index()).copied()
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

impl Default for XdpTable {
    fn default() -> Self {
        Self::new()
    }
}
