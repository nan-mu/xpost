//! Append-only session transcript + live reply lookup.
//!
//! UART/blob bytes live in stable [`Box`] slabs (append-only). File tee is a copy.
//! [`wait_reply`] returns a fully parsed [`OkReply`] (no ReplyGuard).

mod find;

pub(crate) use find::Found;

use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;

use crate::client::ops::FlashEvent;
use crate::r#type::command::Command;
use crate::r#type::reply::OkReply;

/// Shared transcript (internal to [`crate::client::SerialHandler`]).
pub(crate) type SharedRecord = Arc<SharedRecordInner>;

pub(crate) struct SharedRecordInner {
    inner: Mutex<Record>,
    notify: Notify,
}

impl SharedRecordInner {
    pub fn create(path: impl AsRef<Path>, tee_stdout: bool) -> io::Result<SharedRecord> {
        Ok(Arc::new(Self {
            inner: Mutex::new(Record::create(path, tee_stdout)?),
            notify: Notify::new(),
        }))
    }

    /// In-memory transcript only (no log file). Test harness under `dev`.
    #[cfg(all(test, feature = "dev"))]
    pub fn create_memory() -> SharedRecord {
        Arc::new(Self {
            inner: Mutex::new(Record::memory()),
            notify: Notify::new(),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, Record> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn uart(&self, line: &[u8]) {
        self.lock().uart(line);
        self.notify.notify_waiters();
    }

    pub fn blob(&self, data: Vec<u8>) {
        self.lock().blob(data);
        self.notify.notify_waiters();
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    /// Wait for a matching reply after `after` (caller races timeout / interrupt).
    pub(crate) async fn wait_reply<E>(
        &self,
        cmd: &Command<'_>,
        after: SystemTime,
    ) -> core::result::Result<Found, E>
    where
        E: crate::client::error::RequestError,
    {
        find::wait_reply(self, cmd, after).await
    }

    /// Build [`OkReply`] borrowing stable slab bytes. Caller must hold `&'a` to the
    /// handler that owns this record; store is append-only so indices stay valid.
    pub(crate) fn materialize_ok<'a, E>(
        &'a self,
        found: &Found,
    ) -> crate::client::error::Result<'a, E>
    where
        E: crate::client::error::RequestError,
    {
        let guard = self.lock();
        let ok = guard.ok_from_found::<E>(found)?;
        // SAFETY: payloads are `Box<[u8]>` (heap, immovable); store is append-only;
        // `'a` is tied to `&'a self` which outlives the temporary `MutexGuard`.
        Ok(unsafe { core::mem::transmute::<OkReply<'_>, OkReply<'a>>(ok) })
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Entry {
    Uart { at: SystemTime, payload: usize },
    Blob { at: SystemTime, payload: usize },
}

impl Entry {
    pub fn at(&self) -> SystemTime {
        match self {
            Self::Uart { at, .. } | Self::Blob { at, .. } => *at,
        }
    }
}

/// Optional file tee + in-memory stable byte store.
#[derive(Debug)]
pub(crate) struct Record {
    file: Option<File>,
    path: Option<PathBuf>,
    tee_stdout: bool,
    /// Append-only; each `Box` is immovable on the heap.
    payloads: Vec<Box<[u8]>>,
    entries: Vec<Entry>,
}

pub(crate) trait WriteRecord {
    fn write_to(&self, rec: &mut Record);
}

impl WriteRecord for OkReply<'_> {
    fn write_to(&self, rec: &mut Record) {
        rec.cmd_rx(crate::client::serial::ok_label(self));
    }
}

impl WriteRecord for Command<'_> {
    fn write_to(&self, rec: &mut Record) {
        match self {
            // Host-only marker in the transcript — never a UART verb.
            Command::GetInit => rec.cmd_tx(&"get init (host wait)"),
            other => rec.cmd_tx(other),
        }
    }
}

impl WriteRecord for FlashEvent {
    fn write_to(&self, rec: &mut Record) {
        rec.flash(self);
    }
}

impl Record {
    pub fn create(path: impl AsRef<Path>, tee_stdout: bool) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)?;
        Ok(Self {
            file: Some(file),
            path: Some(path),
            tee_stdout,
            payloads: Vec::new(),
            entries: Vec::new(),
        })
    }

    /// Stable slabs only; file tee disabled.
    #[cfg(all(test, feature = "dev"))]
    pub fn memory() -> Self {
        Self {
            file: None,
            path: None,
            tee_stdout: false,
            payloads: Vec::new(),
            entries: Vec::new(),
        }
    }

    pub fn payload(&self, i: usize) -> Option<&[u8]> {
        self.payloads.get(i).map(|b| &**b)
    }

    pub fn entry(&self, i: usize) -> Option<&Entry> {
        self.entries.get(i)
    }

    pub fn indices_after(&self, after: SystemTime) -> impl Iterator<Item = usize> + '_ {
        self.entries
            .iter()
            .enumerate()
            .filter(move |(_, e)| e.at() > after)
            .map(|(i, _)| i)
    }

    pub fn cmd_tx(&mut self, cmd: &impl Display) {
        self.append_file(&format!("[cmd] → {cmd}"));
    }

    pub fn cmd_rx(&mut self, reply: &str) {
        self.append_file(&format!("[cmd] ← {reply}"));
    }

    pub fn uart(&mut self, line: &[u8]) {
        let at = SystemTime::now();
        let shown = String::from_utf8_lossy(line);
        self.append_file_at(at, &format!("[uart] {shown}"));
        let payload = self.push_payload(line);
        self.entries.push(Entry::Uart { at, payload });
    }

    pub fn blob(&mut self, data: Vec<u8>) {
        let at = SystemTime::now();
        let payload = self.push_payload(&data);
        self.entries.push(Entry::Blob { at, payload });
    }

    pub fn flash(&mut self, flash: &impl Display) {
        self.append_file(&flash.to_string());
    }

    fn push_payload(&mut self, bytes: &[u8]) -> usize {
        let i = self.payloads.len();
        self.payloads.push(bytes.to_vec().into_boxed_slice());
        i
    }

    fn append_file(&mut self, line: &str) {
        self.append_file_at(SystemTime::now(), line);
    }

    fn append_file_at(&mut self, at: SystemTime, line: &str) {
        let ts = rfc3339_millis(at);
        let full = format!("{ts} {line}");
        if let Some(file) = self.file.as_mut() {
            let _ = writeln!(file, "{full}");
            let _ = file.flush();
        }
        if self.tee_stdout {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{full}");
            let _ = out.flush();
        }
    }

    pub fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> io::Result<Vec<String>> {
        let Some(path) = self.path.as_ref() else {
            return Ok(Vec::new());
        };
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let Some(ts) = parse_leading_rfc3339_millis(&line) else {
                continue;
            };
            if ts >= since && ts <= until {
                out.push(line);
            }
        }
        Ok(out)
    }

    pub(crate) fn ok_from_found<E>(
        &self,
        found: &Found,
    ) -> core::result::Result<OkReply<'_>, E>
    where
        E: From<crate::client::error::ParseError>,
    {
        Ok(match found {
            Found::Ping => OkReply::Ping,
            Found::EnableXdp => OkReply::EnableXdp,
            Found::DisableXdp => OkReply::DisableXdp,
            Found::ClearMap => OkReply::ClearMap,
            Found::ClearPerf => OkReply::ClearPerf,
            Found::UpdateMap => OkReply::UpdateMap,
            Found::GetMap { blob_idx } => OkReply::GetMap {
                blob: self.blob_at(*blob_idx)?,
            },
            Found::GetPerf { blob_idx } => OkReply::GetPerf {
                blob: self.blob_at(*blob_idx)?,
            },
            Found::Init => OkReply::Init,
        })
    }

    fn blob_at<E>(&self, blob_idx: usize) -> core::result::Result<&[u8], E>
    where
        E: From<crate::client::error::ParseError>,
    {
        use crate::client::error::ParseError;
        let Entry::Blob { payload, .. } = self
            .entry(blob_idx)
            .ok_or_else(|| ParseError::BadShape("missing-blob").into())?
        else {
            return Err(ParseError::BadShape("not-blob").into());
        };
        self.payload(*payload)
            .ok_or_else(|| ParseError::BadShape("missing-payload").into())
    }
}

fn rfc3339_millis(ts: SystemTime) -> String {
    let Ok(dur) = ts.duration_since(UNIX_EPOCH) else {
        return "1970-01-01T00:00:00.000Z".into();
    };
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    let days = secs / 86400;
    let tod = secs % 86400;
    let h = tod / 3600;
    let m = (tod % 3600) / 60;
    let s = tod % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

fn parse_leading_rfc3339_millis(line: &str) -> Option<SystemTime> {
    let stamp = line.split_whitespace().next()?;
    if stamp.len() != 24 || !stamp.ends_with('Z') {
        return None;
    }
    let y: i64 = stamp.get(0..4)?.parse().ok()?;
    let mo: u32 = stamp.get(5..7)?.parse().ok()?;
    let d: u32 = stamp.get(8..10)?.parse().ok()?;
    let h: u64 = stamp.get(11..13)?.parse().ok()?;
    let mi: u64 = stamp.get(14..16)?.parse().ok()?;
    let s: u64 = stamp.get(17..19)?.parse().ok()?;
    let ms: u64 = stamp.get(20..23)?.parse().ok()?;
    let days = days_from_civil(y, mo, d)?;
    let secs = days * 86400 + h * 3600 + mi * 60 + s;
    Some(UNIX_EPOCH + std::time::Duration::from_millis(secs * 1000 + ms))
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> Option<u64> {
    if !(1..=12).contains(&m) || d == 0 || d > 31 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m as u64 - 3 } else { m as u64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let z = era * 146097 + doe as i64 - 719468;
    if z < 0 {
        return None;
    }
    Some(z as u64)
}
