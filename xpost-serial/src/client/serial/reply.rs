//! Line → [`crate::r#type::Reply`]; collect multi-line `get map`.

pub mod perf;

use std::time::{Duration, Instant};

use crate::r#type::reply::{
    ErrReply, MapDump, OkReply, ParseReplyError, Reply, is_get_map_end, is_reply_prefix,
    parse_get_map_header, parse_map_entry, parse_reply_line,
};

use crate::client::error::{Error, Result};
use crate::client::serial::uart::Event;

/// Reply plus optional binary payload (`get perf`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyMsg {
    pub reply: Reply,
    pub blob: Option<Vec<u8>>,
}

type Wait<'a> = dyn FnMut(Duration) -> Result<Event> + 'a;

fn remap_timeout(err: Error, full: Duration) -> Error {
    match err {
        Error::RequestTimeout(_) => Error::RequestTimeout(full),
        other => other,
    }
}

/// Wait for a single-line `OK`/`ERR` (or `get perf` header+blob / `get map` dump).
///
/// `wait` must be a block-select: business / interrupt / timeout (see [`crate::client::serial::uart::Uart::select`]).
pub fn collect_reply(
    wait: &mut Wait<'_>,
    want_map: bool,
    want_perf: bool,
    timeout: Duration,
) -> Result<ReplyMsg> {
    if want_map {
        return collect_get_map(wait, timeout);
    }
    if want_perf {
        return collect_get_perf(wait, timeout);
    }
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::RequestTimeout(timeout));
        }
        match wait(remaining).map_err(|e| remap_timeout(e, timeout))? {
            Event::Blob(_) => {
                return Err(Error::Msg("unexpected binary blob".into()));
            }
            Event::Line(line) => {
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

fn collect_get_map(wait: &mut Wait<'_>, timeout: Duration) -> Result<ReplyMsg> {
    let deadline = Instant::now() + timeout;
    let mut header: Option<(String, crate::r#type::reply::MapKind)> = None;
    let mut entries = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::RequestTimeout(timeout));
        }
        let Event::Line(line) = wait(remaining).map_err(|e| remap_timeout(e, timeout))? else {
            return Err(Error::Msg("unexpected binary blob during get map".into()));
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

fn collect_get_perf(wait: &mut Wait<'_>, timeout: Duration) -> Result<ReplyMsg> {
    let deadline = Instant::now() + timeout;
    let len = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::RequestTimeout(timeout));
        }
        match wait(remaining).map_err(|e| remap_timeout(e, timeout))? {
            Event::Blob(_) => {
                return Err(Error::Msg("unexpected blob before get perf header".into()));
            }
            Event::Line(line) => {
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
                    Err(e) => {
                        return Err(Error::Msg(format!("bad get perf header `{line}`: {e}")));
                    }
                }
            }
        }
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::RequestTimeout(timeout));
    }
    match wait(remaining).map_err(|e| remap_timeout(e, timeout))? {
        Event::Blob(blob) if blob.len() == len => Ok(ReplyMsg {
            reply: Reply::Ok(OkReply::GetPerf { len }),
            blob: Some(blob),
        }),
        Event::Blob(blob) => Err(Error::Msg(format!(
            "get perf blob len {} != header {len}",
            blob.len()
        ))),
        Event::Line(s) => Err(Error::Msg(format!("expected blob, got line: {s}"))),
    }
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
