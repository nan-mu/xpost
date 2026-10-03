//! `ERR …` reply parsers.
//!
//! General device failures ([`ServerError`]) are command-independent wire lines.
//! Map / perf domain lines are peers and are not folded into [`ServerError`].

use winnow::{
    ascii::space1,
    combinator::opt,
    error::EmptyError,
    Parser,
};

use crate::client::error::ParseError;
use crate::r#type::command::Command;
use crate::r#type::reply::ServerError;

/// Fixed-length `ERR <tok…>` unit reply; optional trailing `\n`.
macro_rules! fixed_err {
    ($(
        $(#[$meta:meta])*
        $fn:ident => $variant:ident : [$($tok:literal),+ $(,)?]
    );+ $(;)?) => {
        $(
            $(#[$meta])*
            fn $fn(input: &[u8]) -> Result<ServerError, ParseError> {
                (
                    b"ERR",
                    $(
                        space1::<_, EmptyError>,
                        $tok,
                    )+
                    opt(b'\n'),
                )
                    .void()
                    .map(|()| ServerError::$variant)
                    .parse(input)
                    .map_err(|_| ParseError::InvalidSyntax)
            }
        )+
    };
}

fixed_err! {
    /// `ERR unknown`
    parse_unknown => Unknown: [b"unknown"];
    /// `ERR timeout`
    parse_timeout => Timeout: [b"timeout"];
    /// `ERR logic`
    parse_logic => Logic: [b"logic"];
    /// `ERR busy`
    parse_busy => Busy: [b"busy"];
    /// `ERR unsupported`
    parse_unsupported => Unsupported: [b"unsupported"];
}

/// Parse a DUT `ERR …` line as a general [`ServerError`].
///
/// Accepts only the five general device lines (`unknown` / `timeout` / `logic` /
/// `busy` / `unsupported`). Map, perf and request-syntax errors are separate
/// domains and yield [`ParseError::InvalidSyntax`] here. `cmd` is reserved for
/// callers that dispatch by outbound command; general failures ignore it.
pub fn parse_err_reply(
    input: &[u8],
    cmd: &Command<'_>,
) -> Result<ServerError, ParseError> {
    let _ = cmd;
    parse_unknown(input)
        .or_else(|_| parse_timeout(input))
        .or_else(|_| parse_logic(input))
        .or_else(|_| parse_busy(input))
        .or_else(|_| parse_unsupported(input))
}
