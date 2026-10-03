//! Live wait for a matching reply after a command timestamp.

use std::time::SystemTime;

use crate::client::error::{ParseError, RequestError};
use crate::client::record::{Entry, SharedRecordInner};
use crate::client::reply;
use crate::r#type::command::Command;
use crate::r#type::reply::{OkReply, ServerError};

/// Indices into the append-only record (no borrows).
pub(crate) enum Found {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap,
    ClearPerf,
    UpdateMap,
    GetMap {
        blob_idx: usize,
    },
    GetPerf {
        blob_idx: usize,
    },
    Init,
}

/// Wait until a matching reply appears after `after` (no internal deadline —
/// caller races this against timeout / interrupt).
pub(crate) async fn wait_reply<E>(
    record: &SharedRecordInner,
    cmd: &Command<'_>,
    after: SystemTime,
) -> core::result::Result<Found, E>
where
    E: RequestError,
{
    loop {
        if let Some(found) = scan_once::<E>(record, cmd, after)? {
            return Ok(found);
        }
        record.notified().await;
    }
}

fn scan_once<E>(
    record: &SharedRecordInner,
    cmd: &Command<'_>,
    after: SystemTime,
) -> core::result::Result<Option<Found>, E>
where
    E: RequestError,
{
    let guard = record.lock();
    match cmd {
        Command::GetMap { .. } => scan_blob_framed::<E>(
            &guard,
            cmd,
            after,
            reply::parse_get_map_len,
            |idx| Found::GetMap { blob_idx: idx },
        ),
        Command::GetPerf => scan_blob_framed::<E>(
            &guard,
            cmd,
            after,
            reply::parse_get_perf_len,
            |idx| Found::GetPerf { blob_idx: idx },
        ),
        other => scan_simple::<E>(&guard, other, after),
    }
}

fn scan_simple<E>(
    guard: &std::sync::MutexGuard<'_, crate::client::record::Record>,
    cmd: &Command<'_>,
    after: SystemTime,
) -> core::result::Result<Option<Found>, E>
where
    E: RequestError,
{
    for idx in guard.indices_after(after) {
        let Some(Entry::Uart { payload, .. }) = guard.entry(idx) else {
            continue;
        };
        let Some(line) = guard.payload(*payload) else {
            continue;
        };
        if let Some(err) = wire_err_line(line, cmd) {
            return Err(err);
        }
        if let Some(found) = match_simple(cmd, line) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// Header line then a following [`Entry::Blob`].
fn scan_blob_framed<E>(
    guard: &std::sync::MutexGuard<'_, crate::client::record::Record>,
    cmd: &Command<'_>,
    after: SystemTime,
    header_len: impl Fn(&[u8]) -> Option<usize>,
    found: impl Fn(usize) -> Found,
) -> core::result::Result<Option<Found>, E>
where
    E: RequestError,
{
    let mut saw_header = false;
    for idx in guard.indices_after(after) {
        match guard.entry(idx) {
            Some(Entry::Uart { payload, .. }) => {
                let Some(line) = guard.payload(*payload) else {
                    continue;
                };
                if let Some(err) = wire_err_line(line, cmd) {
                    return Err(err);
                }
                if header_len(line).is_some() {
                    saw_header = true;
                }
            }
            Some(Entry::Blob { .. }) => {
                if saw_header {
                    return Ok(Some(found(idx)));
                }
            }
            None => {}
        }
    }
    Ok(None)
}

fn wire_err_line<E>(line: &[u8], cmd: &Command<'_>) -> Option<E>
where
    E: From<ParseError> + From<ServerError>,
{
    if !trim_ascii(line).starts_with(b"ERR ") {
        return None;
    }
    match ServerError::try_from((line, cmd)) {
        Ok(e) => Some(e.into()),
        Err(e) => Some(e.into()),
    }
}

fn trim_ascii(line: &[u8]) -> &[u8] {
    let mut s = 0;
    let mut e = line.len();
    while s < e && matches!(line[s], b' ' | b'\t' | b'\n' | b'\r') {
        s += 1;
    }
    while e > s && matches!(line[e - 1], b' ' | b'\t' | b'\n' | b'\r') {
        e -= 1;
    }
    &line[s..e]
}

fn match_simple(cmd: &Command<'_>, line: &[u8]) -> Option<Found> {
    match OkReply::try_from((line, cmd)).ok()? {
        OkReply::Ping => Some(Found::Ping),
        OkReply::EnableXdp => Some(Found::EnableXdp),
        OkReply::DisableXdp => Some(Found::DisableXdp),
        OkReply::ClearMap => Some(Found::ClearMap),
        OkReply::ClearPerf => Some(Found::ClearPerf),
        OkReply::UpdateMap => Some(Found::UpdateMap),
        OkReply::Init => Some(Found::Init),
        OkReply::GetMap { .. } | OkReply::GetPerf { .. } => None,
    }
}
