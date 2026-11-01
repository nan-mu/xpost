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

fn parse_u32_line<'a>(input: &'a [u8], prefix: &[u8], n: usize) -> Result<(&'a [u8], [u32; 10]), ParseError> {
    let line = input.strip_suffix(b"\n").unwrap_or(input);
    let rest = line
        .strip_prefix(prefix)
        .ok_or(ParseError::InvalidSyntax)?;
    let mut nums = [0u32; 10];
    let mut rest = rest;
    for i in 0..n {
        if !rest.starts_with(b" ") {
            return Err(ParseError::InvalidSyntax);
        }
        rest = &rest[1..];
        let (value, next) = take_u32(rest)?;
        nums[i] = value;
        rest = next;
    }
    if !rest.is_empty() {
        return Err(ParseError::InvalidSyntax);
    }
    Ok((line, nums))
}

fn take_u32(input: &[u8]) -> Result<(u32, &[u8]), ParseError> {
    if input.is_empty() || !input[0].is_ascii_digit() {
        return Err(ParseError::InvalidSyntax);
    }
    let mut n: u32 = 0;
    let mut i = 0;
    while i < input.len() && input[i].is_ascii_digit() {
        let d = u32::from(input[i] - b'0');
        n = n
            .checked_mul(10)
            .and_then(|n| n.checked_add(d))
            .ok_or(ParseError::InvalidSyntax)?;
        i += 1;
    }
    Ok((n, &input[i..]))
}

fn u8_at(nums: &[u32; 10], i: usize) -> Result<u8, ParseError> {
    u8::try_from(nums[i]).map_err(|_| ParseError::InvalidSyntax)
}

/// `OK xdp flash <slot> <generation> <mcu_us>`.
pub fn parse_xdp_flash_reply(input: &[u8]) -> Result<OkReply<'_>, ParseError> {
    let (_, nums) = parse_u32_line(input, b"OK xdp flash", 3)?;
    Ok(OkReply::XdpFlash {
        slot: u8_at(&nums, 0)?,
        generation: nums[1],
        mcu_us: nums[2],
    })
}

/// `OK xdp switch` plus the eight commit counters and `mcu_us`.
pub fn parse_xdp_switch_reply(input: &[u8]) -> Result<OkReply<'_>, ParseError> {
    let (_, nums) = parse_u32_line(input, b"OK xdp switch", 9)?;
    Ok(OkReply::XdpSwitch {
        ok: u8_at(&nums, 0)?,
        rollback: u8_at(&nums, 1)?,
        severe: u8_at(&nums, 2)?,
        from_slot: u8_at(&nums, 3)?,
        to_slot: u8_at(&nums, 4)?,
        generation: nums[5],
        quiesce_drop: nums[6],
        quiesce_pass: nums[7],
        mcu_us: nums[8],
    })
}

/// `OK get xdp` plus boot id, slot roles and quiesce counts.
pub fn parse_get_xdp_status_reply(input: &[u8]) -> Result<OkReply<'_>, ParseError> {
    let (_, nums) = parse_u32_line(input, b"OK get xdp", 9)?;
    Ok(OkReply::GetXdpStatus {
        boot_id: nums[0],
        active_slot: u8_at(&nums, 1)?,
        staged_slot: u8_at(&nums, 2)?,
        generation: nums[3],
        updating: u8_at(&nums, 4)?,
        severe: u8_at(&nums, 5)?,
        quiesce_drop: nums[6],
        quiesce_pass: nums[7],
        last_error: u8_at(&nums, 8)?,
    })
}

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
