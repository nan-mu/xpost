//! XDP-facing map ops: enum errors; helpers convert to errno / null.

use log::{debug, error};

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
    /// Log BUG / logic failures. `NotFound` / `Exists` / datapath `Busy` are quiet.
    pub(crate) fn log_if_bug(self, ctx: &str) {
        match self {
            Self::NotFound | Self::Exists => {}
            Self::Busy => debug!("[Helper][MAP][{}]: pool busy", ctx),
            Self::InvalidArg => error!("[Helper][MAP][{}]: invalid argument", ctx),
            Self::NoMem => error!("[Helper][MAP][{}]: out of MAP capacity", ctx),
            Self::Unsupported => error!("[Helper][MAP][{}]: unsupported operation", ctx),
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
