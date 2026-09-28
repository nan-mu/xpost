//! Sync event sink. Implementations must not block for long.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::Event;

const RING_CAP: usize = 8192;

pub trait Sink: Send + Sync {
    fn emit(&self, ev: Event);

    /// Host-timestamped transcript lines with `ts ∈ [since, until]`.
    fn lines_in_range(&self, _since: SystemTime, _until: SystemTime) -> Vec<String> {
        Vec::new()
    }
}

/// Print each [`Event::transcript_line`] to stdout.
#[derive(Debug, Default)]
pub struct StdoutSink;

impl Sink for StdoutSink {
    fn emit(&self, ev: Event) {
        let line = format_line(&ev);
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// Append transcript lines to a file; optionally also print to stdout.
/// Keeps an in-memory ring for [`Sink::lines_in_range`].
#[derive(Debug)]
pub struct FileSink {
    file: Mutex<File>,
    path: PathBuf,
    tee_stdout: bool,
    ring: Mutex<VecDeque<(SystemTime, String)>>,
}

impl FileSink {
    /// Create/truncate `path` and append each transcript line (flush after write).
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
            file: Mutex::new(file),
            path,
            tee_stdout,
            ring: Mutex::new(VecDeque::with_capacity(RING_CAP)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Sink for FileSink {
    fn emit(&self, ev: Event) {
        let line = format_line(&ev);
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
        if let Ok(mut ring) = self.ring.lock() {
            if ring.len() >= RING_CAP {
                ring.pop_front();
            }
            ring.push_back((ev.ts, line.clone()));
        }
        if self.tee_stdout {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }

    fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> Vec<String> {
        let Ok(ring) = self.ring.lock() else {
            return Vec::new();
        };
        ring.iter()
            .filter(|(ts, _)| *ts >= since && *ts <= until)
            .map(|(_, line)| line.clone())
            .collect()
    }
}

/// Collect events in memory (tests / Vec-backed agent).
#[derive(Debug, Default)]
pub struct VecSink {
    events: Mutex<Vec<Event>>,
}

impl VecSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<Event> {
        self.events.lock().expect("vec sink").clone()
    }

    pub fn transcript(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(format_line)
            .collect()
    }
}

impl Sink for VecSink {
    fn emit(&self, ev: Event) {
        self.events.lock().expect("vec sink").push(ev);
    }

    fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> Vec<String> {
        self.events()
            .into_iter()
            .filter(|e| e.ts >= since && e.ts <= until)
            .map(|e| format_line(&e))
            .collect()
    }
}

fn format_line(ev: &Event) -> String {
    format!("{} {}", rfc3339_millis(ev.ts), ev.transcript_line())
}

fn rfc3339_millis(ts: SystemTime) -> String {
    let Ok(dur) = ts.duration_since(UNIX_EPOCH) else {
        return "1970-01-01T00:00:00.000Z".into();
    };
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    // UTC wall clock from unix seconds (no chrono dep).
    let days = secs / 86400;
    let tod = secs % 86400;
    let h = tod / 3600;
    let m = (tod % 3600) / 60;
    let s = tod % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// Howard Hinnant civil_from_days (days since 1970-01-01 → Y-M-D).
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
