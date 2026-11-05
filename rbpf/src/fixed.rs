// SPDX-License-Identifier: (Apache-2.0 OR MIT)
//! Fixed-capacity text for `no_std` messages (`mcu` feature).
//!
//! Upstream `rbpf` builds every message with `format!`, which needs `alloc`.
//! Merely naming `extern crate alloc` forces the linker to demand a
//! `#[global_allocator]`, even when nothing allocates. An MCU runtime must not
//! pay that, so `mcu` builds format into a fixed buffer instead.
//!
//! Capacity is a hard limit: a message that would overrun is truncated. Callers
//! that need a value to survive must keep it short. [`ERR_CAP`] leaves room for
//! the longest diagnostic the interpreter can emit.

use core::fmt::{self, Write};

/// Bytes reserved for one error message. Sized so the interpreter's longest
/// diagnostic (`out of bounds` with an address and a length) fits whole.
pub const ERR_CAP: usize = 128;

/// A `String` that never allocates: writes past capacity are dropped.
#[derive(Clone, Copy)]
pub struct FixedStr<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FixedStr<N> {
    /// An empty buffer.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    /// The written bytes as text. Always valid UTF-8: every write stores either
    /// a `&str` chunk or a `char`, so the buffer never splits a code point.
    pub fn as_str(&self) -> &str {
        // Safety: `write_str` only appends whole `&str` and `char` encodings,
        // both of which are valid UTF-8, and `len` only ever advances by the
        // number of bytes actually copied.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }

    /// Bytes still available.
    pub const fn remaining(&self) -> usize {
        N - self.len
    }
}

impl<const N: usize> Default for FixedStr<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for FixedStr<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let room = N - self.len;
        let take = bytes.len().min(room);
        self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
        self.len += take;
        // A truncated write is not an error here: the message is diagnostic, and
        // the caller cannot retry. Report success so `write!` does not abort.
        Ok(())
    }
}

impl<const N: usize> fmt::Display for FixedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<const N: usize> fmt::Debug for FixedStr<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl<const N: usize> AsRef<str> for FixedStr<N> {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Format a diagnostic message without allocating under `mcu`.
///
/// Two expansions, one call site:
///
/// - `std`: expands to `format!`, yielding a `String`. `std::io::Error` requires
///   `Into<Box<dyn Error + Send + Sync>>`, which a `String` satisfies and a
///   fixed buffer does not.
/// - `mcu`: expands to a fixed-capacity [`FixedStr`]. Naming `alloc::format!`
///   would force a global allocator on the firmware.
///
/// Only diagnostics use this macro. Program values never flow through it.
#[cfg(feature = "std")]
macro_rules! err_fmt {
    ($($arg:tt)*) => {
        format!($($arg)*)
    };
}

/// See the `std` expansion above.
#[cfg(not(feature = "std"))]
macro_rules! err_fmt {
    ($($arg:tt)*) => {{
        let mut s = $crate::fixed::FixedStr::<{ $crate::fixed::ERR_CAP }>::new();
        // Capacity overflow is intentional truncation, not a failure.
        let _ = core::fmt::write(&mut s, format_args!($($arg)*));
        s
    }};
}

pub(crate) use err_fmt;

/// Implementation of `Error` for the `mcu` build. Never allocates.
#[derive(Clone, Copy)]
pub struct Error {
    #[allow(dead_code)]
    kind: ErrorKind,
    message: FixedStr<ERR_CAP>,
}

impl Error {
    /// Same signature as `std::io::Error::new`.
    pub fn new<S: AsRef<str>>(kind: ErrorKind, error: S) -> Error {
        let mut message = FixedStr::new();
        // Capacity overflow truncates. A truncated diagnostic is acceptable;
        // failing to build one is not.
        let _ = Write::write_str(&mut message, error.as_ref());
        Error { kind, message }
    }

    /// The message, truncated if it exceeded [`ERR_CAP`].
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
        f.debug_tuple("Error")
            .field(&self.message.as_str())
            .finish()
    }
}

/// The `mcu` build only produces [`Other`](ErrorKind::Other).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Generic failure.
    Other,
}

impl Error {
    /// Convenience function to create an `Other` kind error.
    pub fn other<S: AsRef<str>>(error: S) -> Error {
        Error::new(ErrorKind::Other, error)
    }
}
