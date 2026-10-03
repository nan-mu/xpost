//! Client reply / host error surfaces.
//!
//! App-mode [`request`](crate::client::SerialHandler::request) uses
//! [`Result`]`<'a, E>` = `Result<OkReply<'a>, E>` with `E: `[`ErrReply`].
//! Open / flash / port I/O use [`std::io::Result`].

use core::fmt;
use std::io;
use std::time::Duration;

use crate::r#type::reply::{ErrReply, MapError, OkReply, PerfError, ServerError, SyntaxError};

/// App-mode request outcome. Callers pick `E: `[`ErrReply`] (and the `From`s
/// required by the request path).
pub type Result<'a, E> = core::result::Result<OkReply<'a>, E>;

/// Local parse failure (slice is not a well-formed OK/ERR reply).
pub enum ParseError {
    NotReply,
    BadShape(&'static str),
    InvalidSyntax,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReply => f.write_str("not-reply"),
            Self::BadShape(why) => write!(f, "bad-shape {why}"),
            Self::InvalidSyntax => f.write_str("invalid-syntax"),
        }
    }
}

impl fmt::Debug for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ParseError {}

impl ErrReply for ParseError {}

impl fmt::Debug for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("Unknown"),
            Self::Timeout => f.write_str("Timeout"),
            Self::Logic => f.write_str("Logic"),
            Self::Busy => f.write_str("Busy"),
            Self::Unsupported => f.write_str("Unsupported"),
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("ERR unknown"),
            Self::Timeout => f.write_str("ERR timeout"),
            Self::Logic => f.write_str("ERR logic"),
            Self::Busy => f.write_str("ERR busy"),
            Self::Unsupported => f.write_str("ERR unsupported"),
        }
    }
}

impl std::error::Error for ServerError {}

impl fmt::Debug for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameNotFound => f.write_str("NameNotFound"),
            Self::KeyNotFound => f.write_str("KeyNotFound"),
            Self::InvalidName => f.write_str("InvalidName"),
            Self::InvalidKey => f.write_str("InvalidKey"),
            Self::InvalidValue => f.write_str("InvalidValue"),
            Self::Full => f.write_str("Full"),
            Self::Unsupported => f.write_str("Unsupported"),
            Self::BufferTooSmall => f.write_str("BufferTooSmall"),
        }
    }
}

impl fmt::Debug for PerfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Unavailable"),
            Self::Unsupported => f.write_str("Unsupported"),
            Self::BufferTooSmall => f.write_str("BufferTooSmall"),
        }
    }
}

impl fmt::Debug for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArity => f.write_str("BadArity"),
            Self::InvalidUtf8 => f.write_str("InvalidUtf8"),
            Self::LineTooLong => f.write_str("LineTooLong"),
        }
    }
}

/// Business-deadline arm of request `select!`.
pub struct RequestTimeout(pub Duration);

impl fmt::Display for RequestTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "request timeout after {:?}", self.0)
    }
}

impl fmt::Debug for RequestTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for RequestTimeout {}

impl Copy for RequestTimeout {}
impl Clone for RequestTimeout {
    fn clone(&self) -> Self {
        *self
    }
}

impl ErrReply for RequestTimeout {}

/// Flash-cancel arm of request `select!`.
pub struct Interrupted;

impl fmt::Display for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("interrupted by flash")
    }
}

impl fmt::Debug for Interrupted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Interrupted")
    }
}

impl std::error::Error for Interrupted {}

impl Copy for Interrupted {}
impl Clone for Interrupted {
    fn clone(&self) -> Self {
        *self
    }
}

impl ErrReply for Interrupted {}

/// Helper bound for App-mode [`request`](crate::client::SerialHandler::request).
pub trait RequestError:
    ErrReply
    + From<ParseError>
    + From<ServerError>
    + From<MapError>
    + From<PerfError>
    + From<SyntaxError>
    + From<RequestTimeout>
    + From<Interrupted>
    + From<io::Error>
{
}

impl<T> RequestError for T where
    T: ErrReply
        + From<ParseError>
        + From<ServerError>
        + From<MapError>
        + From<PerfError>
        + From<SyntaxError>
        + From<RequestTimeout>
        + From<Interrupted>
        + From<io::Error>
{
}
