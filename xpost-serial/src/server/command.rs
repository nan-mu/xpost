//! Borrowing App-mode request parsers built with winnow, without `std` or `alloc`.

use winnow::{
    Parser,
    ascii::Caseless,
    combinator::{eof, terminated},
    error::EmptyError,
    token::take_while,
};

use crate::r#type::{command::Command, reply::ErrReply};

/// Failure while parsing one complete command line.
pub enum ParseCommandError {
    /// No tokens (the UART owner may ignore blank lines).
    Empty,
    /// Unrecognized verb/target, including the host-only `get init`.
    Unknown,
    /// A recognized command has missing or extra arguments.
    BadArity,
    /// A map-name token is not UTF-8; map-domain validation happens later.
    InvalidUtf8,
    /// The UART receive buffer overflowed; discard the whole line.
    TooLong,
}

impl ErrReply for ParseCommandError {}

fn whitespace(input: &mut &[u8]) -> Result<(), EmptyError> {
    take_while(0.., |byte: u8| byte.is_ascii_whitespace())
        .void()
        .parse_next(input)
}

fn token<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], EmptyError> {
    terminated(
        take_while(1.., |byte: u8| !byte.is_ascii_whitespace()),
        whitespace,
    )
    .parse_next(input)
}

fn keyword(input: &[u8], expected: &[u8]) -> bool {
    (Caseless(expected), eof::<_, EmptyError>)
        .parse(input)
        .is_ok()
}

fn name(token: &[u8]) -> Result<&str, ParseCommandError> {
    core::str::from_utf8(token).map_err(|_| ParseCommandError::InvalidUtf8)
}

fn no_args(input: &mut &[u8]) -> Result<(), ParseCommandError> {
    eof::<_, EmptyError>
        .void()
        .parse_next(input)
        .map_err(|_| ParseCommandError::BadArity)
}

fn map_name<'a>(input: &mut &'a [u8]) -> Result<&'a str, ParseCommandError> {
    let raw = terminated(token, eof::<_, EmptyError>)
        .parse_next(input)
        .map_err(|_| ParseCommandError::BadArity)?;
    name(raw)
}

/// Parse one complete command line (a trailing LF/CRLF is optional).
///
/// Winnow consumes ASCII whitespace, whole tokens and the end of input. Verbs
/// and targets ignore ASCII case; map names preserve case. Returned arguments
/// borrow the input. No token collection, heap allocation, or owned error
/// context is used. Missing/extra arguments to a recognized command are rejected.
/// Map-specific key/value validation belongs to the firmware command handler.
/// This is not a UART stream accumulator: pass only one request at a time.
/// Never produces [`Command::GetInit`].
pub fn parse_command(mut line: &[u8]) -> Result<Command<'_>, ParseCommandError> {
    whitespace(&mut line).map_err(|_| ParseCommandError::Empty)?;
    let verb = token(&mut line).map_err(|_| ParseCommandError::Empty)?;
    if keyword(verb, b"ping") {
        no_args(&mut line)?;
        return Ok(Command::Ping);
    }
    let target = token(&mut line).map_err(|_| ParseCommandError::Unknown)?;
    if keyword(verb, b"enable") && keyword(target, b"xdp") {
        no_args(&mut line)?;
        Ok(Command::EnableXdp)
    } else if keyword(verb, b"disable") && keyword(target, b"xdp") {
        no_args(&mut line)?;
        Ok(Command::DisableXdp)
    } else if keyword(verb, b"clear") && keyword(target, b"perf") {
        no_args(&mut line)?;
        Ok(Command::ClearPerf)
    } else if keyword(verb, b"get") && keyword(target, b"perf") {
        no_args(&mut line)?;
        Ok(Command::GetPerf)
    } else if keyword(verb, b"clear") && keyword(target, b"map") {
        Ok(Command::ClearMap {
            name: map_name(&mut line)?,
        })
    } else if keyword(verb, b"get") && keyword(target, b"map") {
        Ok(Command::GetMap {
            name: map_name(&mut line)?,
        })
    } else if keyword(verb, b"update") && keyword(target, b"map") {
        let (raw_name, key, value, _) = (token, token, token, eof::<_, EmptyError>)
            .parse_next(&mut line)
            .map_err(|_| ParseCommandError::BadArity)?;
        Ok(Command::UpdateMap {
            name: name(raw_name)?,
            key,
            value,
        })
    } else {
        Err(ParseCommandError::Unknown)
    }
}
