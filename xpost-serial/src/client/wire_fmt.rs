//! Host-side wire codecs: [`Command`] Display; OK / ERR TryFrom.
//!
//! Reply decode takes the outbound [`Command`] plus a complete reply slice —
//! match only the shape expected for that command (no stream search).
//! Parsers live in [`crate::client::reply`].

use core::fmt;

use crate::{
    client::{
        error::ParseError,
        reply::{
            parse_clear_map_reply, parse_clear_perf_reply, parse_disable_exec_reply,
            parse_disable_xdp_reply, parse_enable_exec_reply, parse_enable_xdp_reply,
            parse_err_reply, parse_finish_reply, parse_get_map_reply, parse_get_perf_reply,
            parse_get_xdp_status_reply, parse_init_reply, parse_map_err_reply,
            parse_perf_err_reply, parse_ping_reply, parse_syntax_err_reply,
            parse_update_map_reply, parse_xdp_activate_reply, parse_xdp_rollback_reply,
            parse_xdp_stage_reply,
        },
    },
    r#type::{
        command::Command,
        reply::{MapError, OkReply, PerfError, ServerError, SyntaxError},
    },
};

impl fmt::Display for Command<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ping => f.write_str("ping"),
            Self::EnableXdp => f.write_str("enable xdp"),
            Self::DisableXdp => f.write_str("disable xdp"),
            Self::EnableExec => f.write_str("enable exec"),
            Self::DisableExec => f.write_str("disable exec"),
            Self::ClearMap { name } => {
                f.write_str("clear map ")?;
                f.write_str(name)
            }
            Self::GetMap { name } => {
                f.write_str("get map ")?;
                f.write_str(name)
            }
            Self::UpdateMap { name, key, value } => {
                f.write_str("update map ")?;
                f.write_str(name)?;
                f.write_str(" ")?;
                write_bytes(f, key)?;
                f.write_str(" ")?;
                write_bytes(f, value)
            }
            Self::ClearPerf => f.write_str("clear perf"),
            Self::GetPerf => f.write_str("get perf"),
            Self::XdpStage { name, slot } => write!(f, "xdp stage {name} {slot}"),
            Self::XdpActivate {
                slot,
                generation,
                pass,
            } => {
                let policy = if *pass { "pass" } else { "drop" };
                write!(f, "xdp activate {slot} {generation} {policy}")
            }
            Self::XdpRollback { pass } => {
                let policy = if *pass { "pass" } else { "drop" };
                write!(f, "xdp rollback {policy}")
            }
            Self::GetXdpStatus => f.write_str("get xdp"),
            // Host-only: must never be serialized to UART.
            Self::GetInit | Self::GetFinish => Err(fmt::Error),
        }
    }
}

impl<'a> TryFrom<(&'a [u8], &Command<'_>)> for OkReply<'a> {
    type Error = ParseError;

    fn try_from((raw, cmd): (&'a [u8], &Command<'_>)) -> Result<Self, Self::Error> {
        match cmd {
            Command::Ping => parse_ping_reply(raw),
            Command::EnableXdp => parse_enable_xdp_reply(raw),
            Command::DisableXdp => parse_disable_xdp_reply(raw),
            Command::EnableExec => parse_enable_exec_reply(raw),
            Command::DisableExec => parse_disable_exec_reply(raw),
            Command::ClearMap { .. } => parse_clear_map_reply(raw),
            Command::ClearPerf => parse_clear_perf_reply(raw),
            Command::UpdateMap { .. } => parse_update_map_reply(raw),
            Command::GetPerf => parse_get_perf_reply(raw),
            Command::GetMap { .. } => parse_get_map_reply(raw),
            Command::XdpStage { .. } => parse_xdp_stage_reply(raw),
            Command::XdpActivate { .. } => parse_xdp_activate_reply(raw),
            Command::XdpRollback { .. } => parse_xdp_rollback_reply(raw),
            Command::GetXdpStatus => parse_get_xdp_status_reply(raw),
            Command::GetInit => parse_init_reply(raw),
            Command::GetFinish => parse_finish_reply(raw),
        }
    }
}

impl<'a> TryFrom<(&'a [u8], &Command<'_>)> for ServerError {
    type Error = ParseError;

    fn try_from((raw, cmd): (&'a [u8], &Command<'_>)) -> Result<Self, Self::Error> {
        parse_err_reply(raw, cmd)
    }
}

impl<'a> TryFrom<(&'a [u8], &Command<'_>)> for MapError {
    type Error = ParseError;

    fn try_from((raw, cmd): (&'a [u8], &Command<'_>)) -> Result<Self, Self::Error> {
        parse_map_err_reply(raw, cmd)
    }
}

impl<'a> TryFrom<(&'a [u8], &Command<'_>)> for PerfError {
    type Error = ParseError;

    fn try_from((raw, cmd): (&'a [u8], &Command<'_>)) -> Result<Self, Self::Error> {
        parse_perf_err_reply(raw, cmd)
    }
}

impl<'a> TryFrom<(&'a [u8], &Command<'_>)> for SyntaxError {
    type Error = ParseError;

    fn try_from((raw, cmd): (&'a [u8], &Command<'_>)) -> Result<Self, Self::Error> {
        parse_syntax_err_reply(raw, cmd)
    }
}

fn write_bytes(f: &mut fmt::Formatter<'_>, b: &[u8]) -> fmt::Result {
    f.write_str(core::str::from_utf8(b).unwrap_or("\u{FFFD}"))
}
