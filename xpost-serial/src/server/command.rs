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

fn parse_u32(token: &[u8]) -> Result<u32, ParseCommandError> {
    if token.is_empty() || token.len() > 10 {
        return Err(ParseCommandError::BadArity);
    }
    let mut n: u32 = 0;
    for b in token {
        let d = b.wrapping_sub(b'0');
        if d > 9 {
            return Err(ParseCommandError::BadArity);
        }
        n = n
            .checked_mul(10)
            .and_then(|n| n.checked_add(u32::from(d)))
            .ok_or(ParseCommandError::BadArity)?;
    }
    Ok(n)
}

fn parse_u8(token: &[u8]) -> Result<u8, ParseCommandError> {
    u8::try_from(parse_u32(token)?).map_err(|_| ParseCommandError::BadArity)
}

fn parse_policy(token: &[u8]) -> Result<bool, ParseCommandError> {
    if keyword(token, b"pass") {
        Ok(true)
    } else if keyword(token, b"drop") {
        Ok(false)
    } else {
        Err(ParseCommandError::BadArity)
    }
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
/// Never produces [`Command::GetInit`] or [`Command::GetFinish`].
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
    } else if keyword(verb, b"enable") && keyword(target, b"exec") {
        no_args(&mut line)?;
        Ok(Command::EnableExec)
    } else if keyword(verb, b"disable") && keyword(target, b"exec") {
        no_args(&mut line)?;
        Ok(Command::DisableExec)
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
    } else if keyword(verb, b"get") && keyword(target, b"xdp") {
        no_args(&mut line)?;
        Ok(Command::GetXdpStatus)
    } else if keyword(verb, b"xdp") && keyword(target, b"flash") {
        let (raw_name, raw_slot, _) = (token, token, eof::<_, EmptyError>)
            .parse_next(&mut line)
            .map_err(|_| ParseCommandError::BadArity)?;
        Ok(Command::XdpFlash {
            name: name(raw_name)?,
            slot: parse_u8(raw_slot)?,
        })
    } else if keyword(verb, b"xdp") && keyword(target, b"switch") {
        let (raw_slot, raw_gen, raw_policy, _) = (token, token, token, eof::<_, EmptyError>)
            .parse_next(&mut line)
            .map_err(|_| ParseCommandError::BadArity)?;
        Ok(Command::XdpSwitch {
            slot: parse_u8(raw_slot)?,
            generation: parse_u32(raw_gen)?,
            pass: parse_policy(raw_policy)?,
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
