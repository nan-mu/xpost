//! Wire requests: one whitespace-tokenized line (ignore-ascii-case).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

/// App-mode line command (DUT UART).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap { name: String },
    GetMap { name: String },
    UpdateMap {
        name: String,
        key: String,
        value: String,
    },
    ClearPerf,
    GetPerf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseCommandError {
    Empty,
    Unknown,
    BadArity(&'static str),
}

impl fmt::Display for ParseCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty"),
            Self::Unknown => write!(f, "unknown"),
            Self::BadArity(cmd) => write!(f, "bad-arity {cmd}"),
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ping => write!(f, "ping"),
            Self::EnableXdp => write!(f, "enable xdp"),
            Self::DisableXdp => write!(f, "disable xdp"),
            Self::ClearMap { name } => write!(f, "clear map {name}"),
            Self::GetMap { name } => write!(f, "get map {name}"),
            Self::UpdateMap { name, key, value } => {
                write!(f, "update map {name} {key} {value}")
            }
            Self::ClearPerf => write!(f, "clear perf"),
            Self::GetPerf => write!(f, "get perf"),
        }
    }
}

/// Parse one command line (no trailing newline required).
pub fn parse_command(line: &str) -> Result<Command, ParseCommandError> {
    let line = line.trim();
    if line.is_empty() {
        return Err(ParseCommandError::Empty);
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(ParseCommandError::Empty);
    }

    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

    match tokens.as_slice() {
        [a] if eq(a, "ping") => Ok(Command::Ping),
        [a, b] if eq(a, "enable") && eq(b, "xdp") => Ok(Command::EnableXdp),
        [a, b] if eq(a, "disable") && eq(b, "xdp") => Ok(Command::DisableXdp),
        [a, b] if eq(a, "clear") && eq(b, "perf") => Ok(Command::ClearPerf),
        [a, b] if eq(a, "get") && eq(b, "perf") => Ok(Command::GetPerf),
        [a, b, name] if eq(a, "clear") && eq(b, "map") => Ok(Command::ClearMap {
            name: (*name).to_string(),
        }),
        [a, b, name] if eq(a, "get") && eq(b, "map") => Ok(Command::GetMap {
            name: (*name).to_string(),
        }),
        [a, b, name, key, value] if eq(a, "update") && eq(b, "map") => Ok(Command::UpdateMap {
            name: (*name).to_string(),
            key: (*key).to_string(),
            value: (*value).to_string(),
        }),
        [a, b, ..] if eq(a, "clear") && eq(b, "map") => Err(ParseCommandError::BadArity("clear map")),
        [a, b, ..] if eq(a, "get") && eq(b, "map") => Err(ParseCommandError::BadArity("get map")),
        [a, b, ..] if eq(a, "update") && eq(b, "map") => {
            Err(ParseCommandError::BadArity("update map"))
        }
        [a, b, ..] if eq(a, "enable") && eq(b, "xdp") => {
            Err(ParseCommandError::BadArity("enable xdp"))
        }
        [a, b, ..] if eq(a, "disable") && eq(b, "xdp") => {
            Err(ParseCommandError::BadArity("disable xdp"))
        }
        [a, b, ..] if eq(a, "clear") && eq(b, "perf") => {
            Err(ParseCommandError::BadArity("clear perf"))
        }
        [a, b, ..] if eq(a, "get") && eq(b, "perf") => Err(ParseCommandError::BadArity("get perf")),
        [a, ..] if eq(a, "ping") => Err(ParseCommandError::BadArity("ping")),
        _ => Err(ParseCommandError::Unknown),
    }
}
