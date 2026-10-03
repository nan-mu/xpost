//! Client reply / host error surfaces.
//!
//! App-mode [`request`](crate::client::SerialHandler::request) uses
//! [`Result`]`<'a, E>` = `Result<OkReply<'a>, E>` with `E: `[`ErrReply`].
//! Open / flash / port I/O use [`std::io::Result`].

use core::fmt;
use std::io;
use std::time::Duration;

use crate::r#type::reply::{ErrReply, OkReply, ServerError};

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
        }
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("ERR unknown"),
        }
    }
}

impl std::error::Error for ServerError {}

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
    + From<RequestTimeout>
    + From<Interrupted>
    + From<io::Error>
{
}

impl<T> RequestError for T where
    T: ErrReply
        + From<ParseError>
        + From<ServerError>
        + From<RequestTimeout>
        + From<Interrupted>
        + From<io::Error>
{
}
