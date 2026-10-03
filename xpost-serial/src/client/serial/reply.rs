//! Line → [`crate::r#type::Reply`]; collect multi-line `get map`.

pub mod perf;

use std::time::{Duration, Instant};

use crate::r#type::reply::{
    ErrReply, MapDump, OkReply, ParseReplyError, Reply, is_get_map_end, is_reply_prefix,
    parse_get_map_header, parse_map_entry, parse_reply_line,
};

use crate::client::error::{Error, Result};

/// Cap per-read wait so flash interrupt can run between polls (RefCell / port release).
const POLL_SLICE: Duration = Duration::from_millis(50);

/// Reply plus optional binary payload (`get perf`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyMsg {
    pub reply: Reply,
    pub blob: Option<Vec<u8>>,
}

type ReadLine<'a> = dyn FnMut(Duration) -> Result<Option<String>> + 'a;
type ReadExact<'a> = dyn FnMut(usize, Duration) -> Result<Vec<u8>> + 'a;

fn slice(remaining: Duration) -> Duration {
    if remaining < POLL_SLICE {
        remaining
    } else {
        POLL_SLICE
    }
}

/// Wait for a single-line `OK`/`ERR` (or `get perf` header+blob / `get map` dump).
pub fn collect_reply(
    read_line: &mut ReadLine<'_>,
    read_exact: &mut ReadExact<'_>,
    want_map: bool,
    want_perf: bool,
    timeout: Duration,
    interrupt: &mut dyn FnMut() -> bool,
) -> Result<ReplyMsg> {
    if want_map {
        return collect_get_map(read_line, timeout, interrupt);
    }
    if want_perf {
        return collect_get_perf(read_line, read_exact, timeout, interrupt);
    }
    let deadline = Instant::now() + timeout;
    loop {
        if interrupt() {
            return Err(Error::Interrupted);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::RequestTimeout(timeout));
        }
        match read_line(slice(remaining))? {
            None => continue,
            Some(line) => {
                if !is_reply_prefix(&line) {
                    continue;
                }
                match parse_reply_line(&line) {
                    Ok(reply) => return Ok(ReplyMsg { reply, blob: None }),
                    Err(ParseReplyError::BadShape("get-map-multiline")) => {
                        return Err(Error::Msg(
                            "unexpected get map header on single-line wait".into(),
                        ));
                    }
                    Err(e) => {
                        return Err(Error::Msg(format!("bad reply `{line}`: {e}")));
                    }
                }
            }
        }
    }
}

fn collect_get_map(
    read_line: &mut ReadLine<'_>,
    timeout: Duration,
    interrupt: &mut dyn FnMut() -> bool,
) -> Result<ReplyMsg> {
    let deadline = Instant::now() + timeout;
    let mut header: Option<(String, crate::r#type::reply::MapKind)> = None;
    let mut entries = Vec::new();
    loop {
        if interrupt() {
            return Err(Error::Interrupted);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::RequestTimeout(timeout));
        }
        let Some(line) = read_line(slice(remaining))? else {
            continue;
        };
        if line.starts_with("ERR ") {
            let reply = parse_reply_line(&line)
                .map_err(|e| Error::Msg(format!("bad err `{line}`: {e}")))?;
            return Ok(ReplyMsg { reply, blob: None });
        }
        if header.is_none() {
            if !is_reply_prefix(&line) {
                continue;
            }
            match parse_get_map_header(&line) {
                Ok(h) => {
                    header = Some(h);
                    continue;
                }
                Err(_) => {
                    let reply = parse_reply_line(&line)
                        .map_err(|e| Error::Msg(format!("expected get map header: {e}")))?;
                    return Ok(ReplyMsg { reply, blob: None });
                }
            }
        }
        if is_get_map_end(&line) {
            let (name, kind) = header.take().unwrap();
            return Ok(ReplyMsg {
                reply: Reply::Ok(OkReply::GetMap(MapDump {
                    name,
                    kind,
                    entries,
                })),
                blob: None,
            });
        }
        if let Some(e) = parse_map_entry(&line) {
            entries.push(e);
            continue;
        }
        if line.starts_with("ERR ") {
            let reply = parse_reply_line(&line)
                .map_err(|e| Error::Msg(format!("bad err `{line}`: {e}")))?;
            return Ok(ReplyMsg { reply, blob: None });
        }
    }
}

fn collect_get_perf(
    read_line: &mut ReadLine<'_>,
    read_exact: &mut ReadExact<'_>,
    timeout: Duration,
    interrupt: &mut dyn FnMut() -> bool,
) -> Result<ReplyMsg> {
    let deadline = Instant::now() + timeout;
    let len = loop {
        if interrupt() {
            return Err(Error::Interrupted);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::RequestTimeout(timeout));
        }
        let Some(line) = read_line(slice(remaining))? else {
            continue;
        };
        if !is_reply_prefix(&line) {
            continue;
        }
        if line.starts_with("ERR ") {
            let reply = parse_reply_line(&line)
                .map_err(|e| Error::Msg(format!("bad err `{line}`: {e}")))?;
            return Ok(ReplyMsg { reply, blob: None });
        }
        match parse_reply_line(&line) {
            Ok(Reply::Ok(OkReply::GetPerf { len })) => break len,
            Ok(other) => {
                return Err(Error::Msg(format!(
                    "expected OK get perf, got {other}"
                )));
            }
            Err(e) => return Err(Error::Msg(format!("bad get perf header `{line}`: {e}"))),
        }
    };
    if interrupt() {
        return Err(Error::Interrupted);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::RequestTimeout(timeout));
    }
    let blob = read_exact(len, remaining)?;
    if blob.len() != len {
        return Err(Error::Msg(format!(
            "get perf blob len {} != header {len}",
            blob.len()
        )));
    }
    Ok(ReplyMsg {
        reply: Reply::Ok(OkReply::GetPerf { len }),
        blob: Some(blob),
    })
}

/// Build a synthetic ERR for host-side failures (not from DUT).
#[allow(dead_code)]
pub fn host_err(cmd: &str, why: &str) -> ReplyMsg {
    ReplyMsg {
        reply: Reply::Err(ErrReply {
            cmd: cmd.into(),
            why: why.into(),
        }),
        blob: None,
    }
}
