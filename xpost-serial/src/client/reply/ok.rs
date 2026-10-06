//! `OK …` reply parsers.
//!
//! - **Fixed** — unit replies via [`fixed_ok!`] (exact token sequence).
//! - **Variable** — length-prefixed blob replies (`get map` / `get perf`);
//!   signatures live here; bodies still in [`super::map`] / [`super::perf`]
//!   until rewritten.

use winnow::{
    ascii::space1,
    combinator::opt,
    error::EmptyError,
    Parser,
};

use crate::client::error::ParseError;
use crate::r#type::reply::OkReply;

// —— fixed OK ————————————————————————————————————————————————————————————————

/// Fixed-length `OK <tok…>` unit reply; optional trailing `\n`.
macro_rules! fixed_ok {
    ($(
        $(#[$meta:meta])*
        $fn:ident => $variant:ident : [$($tok:literal),+ $(,)?]
    );+ $(;)?) => {
        $(
            $(#[$meta])*
            pub fn $fn(input: &[u8]) -> Result<OkReply<'_>, ParseError> {
                (
                    b"OK",
                    $(
                        space1::<_, EmptyError>,
                        $tok,
                    )+
                    opt(b'\n'),
                )
                    .void()
                    .map(|()| OkReply::$variant)
                    .parse(input)
                    .map_err(|_| ParseError::InvalidSyntax)
            }
        )+
    };
}

fixed_ok! {
    /// `OK ping`
    parse_ping_reply => Ping: [b"ping"];
    /// `OK enable xdp`
    parse_enable_xdp_reply => EnableXdp: [b"enable", b"xdp"];
    /// `OK disable xdp`
    parse_disable_xdp_reply => DisableXdp: [b"disable", b"xdp"];
    /// `OK enable exec`
    parse_enable_exec_reply => EnableExec: [b"enable", b"exec"];
    /// `OK disable exec`
    parse_disable_exec_reply => DisableExec: [b"disable", b"exec"];
    /// `OK clear map`
    parse_clear_map_reply => ClearMap: [b"clear", b"map"];
    /// `OK clear perf`
    parse_clear_perf_reply => ClearPerf: [b"clear", b"perf"];
    /// `OK update map`
    parse_update_map_reply => UpdateMap: [b"update", b"map"];
    /// `OK init` — unsolicited settle line (not a command reply).
    parse_init_reply => Init: [b"init"];
    /// `OK finish` — unsolicited completion line (e.g. eBPF printk; not a command reply).
    parse_finish_reply => Finish: [b"finish"];
}

// —— variable OK (length-prefixed blob) ——————————————————————————————————————

/// `OK get map <n>\n` + `n` blob bytes.
///
/// Temporary forward to [`super::map`]; will be rewritten in this module.
pub fn parse_get_map_reply(input: &[u8]) -> Result<OkReply<'_>, ParseError> {
    super::map::parse_ok_get_map(input)
}

/// Declared length from an `OK get map <n>` header line (framing only).
pub fn parse_get_map_len(line: &[u8]) -> Option<usize> {
    super::map::parse_ok_get_map_len(line)
}

/// `OK get perf <n>\n` + `n` blob bytes.
///
/// Temporary forward to [`super::perf`]; will be rewritten in this module.
pub fn parse_get_perf_reply(input: &[u8]) -> Result<OkReply<'_>, ParseError> {
    super::perf::parse_ok_get_perf(input)
}

/// Declared length from an `OK get perf <n>` header line (framing only).
pub fn parse_get_perf_len(line: &[u8]) -> Option<usize> {
    super::perf::parse_ok_get_perf_len(line)
}
