// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! `no_std` `Error` that never allocates.
//!
//! Upstream's `no_std` error stores a `String`, so every `rbpf` error allocates
//! and the crate needs a global allocator. This version stores a fixed-capacity
//! buffer instead, which is what lets the `mcu` build drop `alloc` entirely.
//!
//! The interface stays compatible with the `std` variants: `Error::new` and
//! `Error::other` keep their signatures, and `Display` renders the message.

use crate::fixed::{ERR_CAP, FixedStr};

/// Implementation of `Error` for `no_std` and `mcu` builds.
#[derive(Clone, Copy)]
pub struct Error {
    #[allow(dead_code)]
    kind: ErrorKind,
    message: FixedStr<ERR_CAP>,
}

impl Error {
    /// New function exposing the same signature as `std::io::Error::new`.
    pub fn new<S: AsRef<str>>(kind: ErrorKind, error: S) -> Error {
        let mut message = FixedStr::new();
        // Capacity overflow truncates. A truncated diagnostic is acceptable;
        // failing to build one is not.
        let _ = core::fmt::Write::write_str(&mut message, error.as_ref());
        Error { kind, message }
    }

    /// The message without the `[Verifier]`-style prefix, truncated if long.
    pub fn message(&self) -> &str {
        self.message.as_str()
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.message.as_str())
    }
}

impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Error").field(&self.message.as_str()).finish()
    }
}

/// The current version of `rbpf` only uses the [`Other`](ErrorKind::Other) variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The `no_std` code only uses this variant.
    Other,
}

impl Error {
    /// Convenience function to create an `Other` kind error.
    pub fn other<S: AsRef<str>>(error: S) -> Error {
        Error::new(ErrorKind::Other, error)
    }
}
