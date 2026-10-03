//! Wire replies: `OK …` / `ERR …` (+ multi-line `get map`, binary `get perf`).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

/// Successful App-mode reply (text). Binary `get perf` payload is out-of-band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OkReply {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap,
    /// Full dump including body lines (header already parsed into fields).
    GetMap(MapDump),
    UpdateMap {
        /// Present when host batch updates (`n=<N>`); single update is `None`.
        n: Option<u32>,
    },
    ClearPerf,
    /// Header only; exactly `len` binary bytes follow on the wire.
    GetPerf {
        len: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDump {
    pub name: String,
    pub kind: MapKind,
    pub entries: Vec<MapEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapKind {
    Array { size: u32 },
    Hash { size: u32, cap: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapEntry {
    /// Slot decimal or dotted IPv4.
    pub key: String,
    pub value: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrReply {
    /// Command-ish token(s), e.g. `clear map`, `get perf`.
    pub cmd: String,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Ok(OkReply),
    Err(ErrReply),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseReplyError {
    NotReply,
    BadShape(&'static str),
}

impl fmt::Display for ParseReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotReply => write!(f, "not-reply"),
            Self::BadShape(s) => write!(f, "bad-shape {s}"),
        }
    }
}

impl fmt::Display for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok(ok) => ok.fmt(f),
            Self::Err(err) => err.fmt(f),
        }
    }
}

impl fmt::Display for OkReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ping => write!(f, "OK ping"),
            Self::EnableXdp => write!(f, "OK enable xdp"),
            Self::DisableXdp => write!(f, "OK disable xdp"),
            Self::ClearMap => write!(f, "OK clear map"),
            Self::GetMap(dump) => {
                // Multi-line: header + entries + end.
                dump.fmt(f)
            }
            Self::UpdateMap { n: None } => write!(f, "OK update map"),
            Self::UpdateMap { n: Some(n) } => write!(f, "OK update map n={n}"),
            Self::ClearPerf => write!(f, "OK clear perf"),
            Self::GetPerf { len } => write!(f, "OK get perf {len}"),
        }
    }
}

impl fmt::Display for MapDump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            MapKind::Array { size } => {
                write!(f, "OK get map {} type=array size={size}", self.name)?;
            }
            MapKind::Hash { size, cap } => {
                write!(
                    f,
                    "OK get map {} type=hash size={size} cap={cap}",
                    self.name
                )?;
            }
        }
        for e in &self.entries {
            write!(f, "\n{} {}", e.key, e.value)?;
        }
        write!(f, "\nOK get map end")
    }
}

impl fmt::Display for ErrReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.why.is_empty() {
            write!(f, "ERR {}", self.cmd)
        } else {
            write!(f, "ERR {} {}", self.cmd, self.why)
        }
    }
}

/// Parse a single `OK` / `ERR` line that is not a multi-line `get map` body.
///
/// `OK get map …` headers (non-end) return [`ParseReplyError::BadShape`] — collect
/// via the serial client. `OK get map end` alone is not a complete reply.
pub fn parse_reply_line(line: &str) -> Result<Reply, ParseReplyError> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("ERR ") {
        let rest = rest.trim();
        let mut parts = rest.split_whitespace();
        let Some(first) = parts.next() else {
            return Err(ParseReplyError::BadShape("err"));
        };
        // cmd-ish may be one or two tokens (`clear` / `clear map` / `get perf`).
        let (cmd, why) = match (first, parts.next()) {
            ("clear" | "get" | "update" | "enable" | "disable", Some(second))
                if matches!(second, "map" | "perf" | "xdp") =>
            {
                let cmd = alloc::format!("{first} {second}");
                let why = parts.collect::<Vec<_>>().join(" ");
                if why.is_empty() {
                    return Err(ParseReplyError::BadShape("err-why"));
                }
                (cmd, why)
            }
            (cmd, Some(w)) => {
                let mut why_parts = Vec::new();
                why_parts.push(w);
                why_parts.extend(parts);
                (cmd.to_string(), why_parts.join(" "))
            }
            (cmd, None) => (cmd.to_string(), String::new()),
        };
        return Ok(Reply::Err(ErrReply { cmd, why }));
    }
    if !line.starts_with("OK ") {
        return Err(ParseReplyError::NotReply);
    }
    let rest = line[3..].trim();
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);

    match tokens.as_slice() {
        [a] if eq(a, "ping") => Ok(Reply::Ok(OkReply::Ping)),
        [a, b] if eq(a, "enable") && eq(b, "xdp") => Ok(Reply::Ok(OkReply::EnableXdp)),
        [a, b] if eq(a, "disable") && eq(b, "xdp") => Ok(Reply::Ok(OkReply::DisableXdp)),
        [a, b] if eq(a, "clear") && eq(b, "map") => Ok(Reply::Ok(OkReply::ClearMap)),
        [a, b] if eq(a, "clear") && eq(b, "perf") => Ok(Reply::Ok(OkReply::ClearPerf)),
        [a, b] if eq(a, "update") && eq(b, "map") => {
            Ok(Reply::Ok(OkReply::UpdateMap { n: None }))
        }
        [a, b, n] if eq(a, "update") && eq(b, "map") => {
            let Some(num) = n.strip_prefix("n=").or_else(|| n.strip_prefix("N=")) else {
                return Err(ParseReplyError::BadShape("update-n"));
            };
            let n = num
                .parse()
                .map_err(|_| ParseReplyError::BadShape("update-n"))?;
            Ok(Reply::Ok(OkReply::UpdateMap { n: Some(n) }))
        }
        [a, b, n] if eq(a, "get") && eq(b, "perf") => {
            let len = n
                .parse()
                .map_err(|_| ParseReplyError::BadShape("get-perf-len"))?;
            Ok(Reply::Ok(OkReply::GetPerf { len }))
        }
        [a, b, ..] if eq(a, "get") && eq(b, "map") => {
            // Multi-line — caller must collect.
            Err(ParseReplyError::BadShape("get-map-multiline"))
        }
        _ => Err(ParseReplyError::BadShape("ok")),
    }
}

/// Parse `OK get map <name> type=…` header into name + kind.
pub fn parse_get_map_header(line: &str) -> Result<(String, MapKind), ParseReplyError> {
    let line = line.trim();
    let rest = line
        .strip_prefix("OK get map ")
        .or_else(|| {
            if line.len() >= 11 && line[..11].eq_ignore_ascii_case("OK get map ") {
                Some(&line[11..])
            } else {
                None
            }
        })
        .ok_or(ParseReplyError::BadShape("get-map-header"))?;
    let mut parts = rest.split_whitespace();
    let name = parts
        .next()
        .ok_or(ParseReplyError::BadShape("get-map-name"))?
        .to_string();
    let mut typ = None;
    let mut size = None;
    let mut cap = None;
    for p in parts {
        if let Some(v) = p.strip_prefix("type=") {
            typ = Some(v.to_ascii_lowercase());
        } else if let Some(v) = p.strip_prefix("size=") {
            size = Some(
                v.parse::<u32>()
                    .map_err(|_| ParseReplyError::BadShape("get-map-size"))?,
            );
        } else if let Some(v) = p.strip_prefix("cap=") {
            cap = Some(
                v.parse::<u32>()
                    .map_err(|_| ParseReplyError::BadShape("get-map-cap"))?,
            );
        }
    }
    let size = size.ok_or(ParseReplyError::BadShape("get-map-size"))?;
    let kind = match typ.as_deref() {
        Some("array") => MapKind::Array { size },
        Some("hash") => MapKind::Hash {
            size,
            cap: cap.ok_or(ParseReplyError::BadShape("get-map-cap"))?,
        },
        _ => return Err(ParseReplyError::BadShape("get-map-type")),
    };
    Ok((name, kind))
}

pub fn is_get_map_end(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("OK get map end")
}

pub fn is_reply_prefix(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("OK ") || t.starts_with("ERR ")
}

/// Parse map body line: `<key> <value>` (slot or IPv4 + u64).
pub fn parse_map_entry(line: &str) -> Option<MapEntry> {
    let mut parts = line.split_whitespace();
    let key = parts.next()?.to_string();
    let value = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(MapEntry { key, value })
}

/// `OK get perf <n>` length helper (alias of parsing GetPerf).
pub fn parse_ok_get_perf_len(line: &str) -> Option<usize> {
    match parse_reply_line(line).ok()? {
        Reply::Ok(OkReply::GetPerf { len }) => Some(len),
        _ => None,
    }
}
