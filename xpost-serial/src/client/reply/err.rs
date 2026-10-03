//! `ERR …` reply parsers for flat wire-error domains.
//!
//! Domains are peers: [`ServerError`], [`MapError`], [`PerfError`], [`SyntaxError`].
//! None wraps another. Call the domain parser that matches the expected line.

use winnow::{
    ascii::space1,
    combinator::opt,
    error::EmptyError,
    Parser,
};

use crate::client::error::ParseError;
use crate::r#type::command::Command;
use crate::r#type::reply::{MapError, PerfError, ServerError, SyntaxError};

/// Fixed-length `ERR <tok…>` unit reply; optional trailing `\n`.
macro_rules! fixed_err {
    ($err:ty, $(
        $(#[$meta:meta])*
        $fn:ident => $variant:ident : [$($tok:literal),+ $(,)?]
    );+ $(;)?) => {
        $(
            $(#[$meta])*
            fn $fn(input: &[u8]) -> Result<$err, ParseError> {
                (
                    b"ERR",
                    $(
                        space1::<_, EmptyError>,
                        $tok,
                    )+
                    opt(b'\n'),
                )
                    .void()
                    .map(|()| <$err>::$variant)
                    .parse(input)
                    .map_err(|_| ParseError::InvalidSyntax)
            }
        )+
    };
}

fixed_err! {
    ServerError,
    parse_unknown => Unknown: [b"unknown"];
    parse_timeout => Timeout: [b"timeout"];
    parse_logic => Logic: [b"logic"];
    parse_busy => Busy: [b"busy"];
    parse_unsupported => Unsupported: [b"unsupported"];
}

fixed_err! {
    MapError,
    parse_map_name_not_found => NameNotFound: [b"map", b"name-not-found"];
    parse_map_key_not_found => KeyNotFound: [b"map", b"key-not-found"];
    parse_map_invalid_name => InvalidName: [b"map", b"invalid-name"];
    parse_map_invalid_key => InvalidKey: [b"map", b"invalid-key"];
    parse_map_invalid_value => InvalidValue: [b"map", b"invalid-value"];
    parse_map_full => Full: [b"map", b"full"];
    parse_map_unsupported => Unsupported: [b"map", b"unsupported"];
    parse_map_buffer_too_small => BufferTooSmall: [b"map", b"buffer-too-small"];
}

fixed_err! {
    PerfError,
    parse_perf_unavailable => Unavailable: [b"perf", b"unavailable"];
    parse_perf_unsupported => Unsupported: [b"perf", b"unsupported"];
    parse_perf_buffer_too_small => BufferTooSmall: [b"perf", b"buffer-too-small"];
}

fixed_err! {
    SyntaxError,
    parse_syntax_bad_arity => BadArity: [b"bad-arity"];
    parse_syntax_invalid_utf8 => InvalidUtf8: [b"invalid-utf8"];
    parse_syntax_line_too_long => LineTooLong: [b"line-too-long"];
}

/// Parse a DUT `ERR …` line as a general [`ServerError`].
pub fn parse_err_reply(input: &[u8], cmd: &Command<'_>) -> Result<ServerError, ParseError> {
    let _ = cmd;
    parse_unknown(input)
        .or_else(|_| parse_timeout(input))
        .or_else(|_| parse_logic(input))
        .or_else(|_| parse_busy(input))
        .or_else(|_| parse_unsupported(input))
}

/// Parse a DUT `ERR map …` line as [`MapError`].
pub fn parse_map_err_reply(input: &[u8], cmd: &Command<'_>) -> Result<MapError, ParseError> {
    let _ = cmd;
    parse_map_name_not_found(input)
        .or_else(|_| parse_map_key_not_found(input))
        .or_else(|_| parse_map_invalid_name(input))
        .or_else(|_| parse_map_invalid_key(input))
        .or_else(|_| parse_map_invalid_value(input))
        .or_else(|_| parse_map_full(input))
        .or_else(|_| parse_map_unsupported(input))
        .or_else(|_| parse_map_buffer_too_small(input))
}

/// Parse a DUT `ERR perf …` line as [`PerfError`].
pub fn parse_perf_err_reply(input: &[u8], cmd: &Command<'_>) -> Result<PerfError, ParseError> {
    let _ = cmd;
    parse_perf_unavailable(input)
        .or_else(|_| parse_perf_unsupported(input))
        .or_else(|_| parse_perf_buffer_too_small(input))
}

/// Parse a DUT request-syntax `ERR …` line as [`SyntaxError`].
pub fn parse_syntax_err_reply(input: &[u8], cmd: &Command<'_>) -> Result<SyntaxError, ParseError> {
    let _ = cmd;
    parse_syntax_bad_arity(input)
        .or_else(|_| parse_syntax_invalid_utf8(input))
        .or_else(|_| parse_syntax_line_too_long(input))
}
