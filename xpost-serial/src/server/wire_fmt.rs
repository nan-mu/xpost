//! Firmware-side wire conversion entry points.
//!
//! Mirrors the client's `wire_fmt`: trait/inherent conversions live here;
//! request parser bodies live in `command`, output helpers in `reply`.
//! Binary replies use explicit byte writers, never text `Display` formatting.

use super::{
    command::{ParseCommandError, parse_command},
    reply::{self, WireError},
};
use crate::r#type::{
    command::Command,
    reply::{MapError, OkReply, PerfError, ServerError},
};

impl<'a> TryFrom<&'a [u8]> for Command<'a> {
    type Error = ParseCommandError;

    fn try_from(line: &'a [u8]) -> Result<Self, Self::Error> {
        parse_command(line)
    }
}

impl OkReply<'_> {
    /// Encode one full success reply to a write-all byte sink without allocation.
    /// See [`reply::write_ok`] for framing and partial-write requirements.
    pub fn write_to<E>(&self, write_all: impl FnMut(&[u8]) -> Result<(), E>) -> Result<(), E> {
        reply::write_ok(self, write_all)
    }
}

impl WireError for ServerError {
    fn wire_line(&self) -> &'static [u8] {
        match self {
            Self::Unknown => b"ERR unknown\n",
            Self::Timeout => b"ERR timeout\n",
            Self::Logic => b"ERR logic\n",
            Self::Busy => b"ERR busy\n",
            Self::Unsupported => b"ERR unsupported\n",
        }
    }
}

impl WireError for MapError {
    fn wire_line(&self) -> &'static [u8] {
        match self {
            Self::NameNotFound => b"ERR map name-not-found\n",
            Self::KeyNotFound => b"ERR map key-not-found\n",
            Self::InvalidName => b"ERR map invalid-name\n",
            Self::InvalidKey => b"ERR map invalid-key\n",
            Self::InvalidValue => b"ERR map invalid-value\n",
            Self::Full => b"ERR map full\n",
            Self::Unsupported => b"ERR map unsupported\n",
            Self::BufferTooSmall => b"ERR map buffer-too-small\n",
        }
    }
}

impl WireError for PerfError {
    fn wire_line(&self) -> &'static [u8] {
        match self {
            Self::Unavailable => b"ERR perf unavailable\n",
            Self::Unsupported => b"ERR perf unsupported\n",
            Self::BufferTooSmall => b"ERR perf buffer-too-small\n",
        }
    }
}

impl WireError for ParseCommandError {
    fn wire_line(&self) -> &'static [u8] {
        match self {
            Self::Empty | Self::Unknown => b"ERR unknown\n",
            Self::BadArity => b"ERR bad-arity\n",
            Self::InvalidUtf8 => b"ERR invalid-utf8\n",
            Self::TooLong => b"ERR line-too-long\n",
        }
    }
}
