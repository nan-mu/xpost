//! XDP-facing map ops: enum errors; helpers convert to errno / null.

/// BPF map operation failure (Linux-aligned errno via [`MapError::errno`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    InvalidArg,
    NotFound,
    Exists,
    NoMem,
    Unsupported,
    Busy,
}

impl MapError {
    /// Soft-fail on the helper path: bump [`crate::xdp::counter`] for bug-class errors.
    /// `NotFound` / `Exists` stay quiet (normal datapath misses).
    pub(crate) fn count_if_bug(self) {
        match self {
            Self::NotFound | Self::Exists => {}
            Self::Busy | Self::InvalidArg | Self::NoMem | Self::Unsupported => {
                crate::xdp::counter::inc_helper_map();
            }
        }
    }

    pub(crate) fn errno(self) -> i32 {
        match self {
            Self::InvalidArg | Self::Unsupported => -22, // EINVAL
            Self::NotFound => -2,                        // ENOENT
            Self::Exists => -17,                         // EEXIST
            Self::NoMem => -12,                          // ENOMEM
            Self::Busy => -11,                           // EAGAIN
        }
    }
}

/// Pointer ABI used by AOT helpers and firmware seed/control paths.
pub trait MapOps {
    fn lookup(&self, key: *const u8) -> Result<*mut u8, MapError>;
    fn update(&mut self, key: *const u8, value: *const u8, flags: u64) -> Result<(), MapError>;
    fn delete(&mut self, key: *const u8) -> Result<(), MapError>;
}
