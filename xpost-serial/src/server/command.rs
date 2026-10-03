//! Server-side command line parse (temporarily stubbed).

use core::fmt;

/// Failure while parsing a command line.
pub enum ParseCommandError {
    Empty,
    Unknown,
    BadArity(&'static str),
}

impl fmt::Display for ParseCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty"),
            Self::Unknown => f.write_str("unknown"),
            Self::BadArity(cmd) => write!(f, "bad-arity {cmd}"),
        }
    }
}

impl fmt::Debug for ParseCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl core::error::Error for ParseCommandError {}

/*
use crate::r#type::command::Command;

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn is_ascii_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Split `line` into at most 5 whitespace-separated tokens (stack slots).
fn tokenize<'a>(line: &'a [u8]) -> (usize, [&'a [u8]; 5]) {
    let mut toks = [&b""[..]; 5];
    let mut n = 0;
    let mut i = 0;
    let len = line.len();
    while i < len {
        while i < len && is_ascii_whitespace(line[i]) {
            i += 1;
        }
        if i >= len {
            break;
        }
        let start = i;
        while i < len && !is_ascii_whitespace(line[i]) {
            i += 1;
        }
        if n < 5 {
            toks[n] = &line[start..i];
            n += 1;
        } else {
            // Extra token — bump count past capacity so arity checks fail.
            n = 6;
            break;
        }
    }
    (n, toks)
}

fn token_str(tok: &[u8]) -> Result<&str, ParseCommandError> {
    core::str::from_utf8(tok).map_err(|_| ParseCommandError::Unknown)
}

/// Parse one command line (no trailing newline required).
pub fn parse_command(line: &[u8]) -> Result<Command<'_>, ParseCommandError> {
    let mut start = 0;
    let mut end = line.len();
    while start < end && is_ascii_whitespace(line[start]) {
        start += 1;
    }
    while end > start && is_ascii_whitespace(line[end - 1]) {
        end -= 1;
    }
    let line = &line[start..end];
    if line.is_empty() {
        return Err(ParseCommandError::Empty);
    }
    let (n, t) = tokenize(line);
    match n {
        1 if eq_ignore_ascii_case(t[0], b"ping") => Ok(Command::Ping),
        2 if eq_ignore_ascii_case(t[0], b"enable") && eq_ignore_ascii_case(t[1], b"xdp") => {
            Ok(Command::EnableXdp)
        }
        2 if eq_ignore_ascii_case(t[0], b"disable") && eq_ignore_ascii_case(t[1], b"xdp") => {
            Ok(Command::DisableXdp)
        }
        2 if eq_ignore_ascii_case(t[0], b"clear") && eq_ignore_ascii_case(t[1], b"perf") => {
            Ok(Command::ClearPerf)
        }
        2 if eq_ignore_ascii_case(t[0], b"get") && eq_ignore_ascii_case(t[1], b"perf") => {
            Ok(Command::GetPerf)
        }
        3 if eq_ignore_ascii_case(t[0], b"clear") && eq_ignore_ascii_case(t[1], b"map") => {
            Ok(Command::ClearMap {
                name: token_str(t[2])?,
            })
        }
        3 if eq_ignore_ascii_case(t[0], b"get") && eq_ignore_ascii_case(t[1], b"map") => {
            Ok(Command::GetMap {
                name: token_str(t[2])?,
            })
        }
        5 if eq_ignore_ascii_case(t[0], b"update") && eq_ignore_ascii_case(t[1], b"map") => {
            Ok(Command::UpdateMap {
                name: token_str(t[2])?,
                key: t[3],
                value: t[4],
            })
        }
        _ if eq_ignore_ascii_case(t[0], b"clear")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"map") =>
        {
            Err(ParseCommandError::BadArity("clear map"))
        }
        _ if eq_ignore_ascii_case(t[0], b"get")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"map") =>
        {
            Err(ParseCommandError::BadArity("get map"))
        }
        _ if eq_ignore_ascii_case(t[0], b"update")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"map") =>
        {
            Err(ParseCommandError::BadArity("update map"))
        }
        _ if eq_ignore_ascii_case(t[0], b"enable")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"xdp") =>
        {
            Err(ParseCommandError::BadArity("enable xdp"))
        }
        _ if eq_ignore_ascii_case(t[0], b"disable")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"xdp") =>
        {
            Err(ParseCommandError::BadArity("disable xdp"))
        }
        _ if eq_ignore_ascii_case(t[0], b"clear")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"perf") =>
        {
            Err(ParseCommandError::BadArity("clear perf"))
        }
        _ if eq_ignore_ascii_case(t[0], b"get")
            && n >= 2
            && eq_ignore_ascii_case(t[1], b"perf") =>
        {
            Err(ParseCommandError::BadArity("get perf"))
        }
        _ if eq_ignore_ascii_case(t[0], b"ping") => Err(ParseCommandError::BadArity("ping")),
        _ => Err(ParseCommandError::Unknown),
    }
}
*/

/// Placeholder while [`parse_command`] is stubbed.
pub fn stub_todo() {}
