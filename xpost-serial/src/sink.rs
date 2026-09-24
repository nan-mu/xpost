//! Sync event sink. Implementations must not block for long.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::event::Event;

pub trait Sink: Send + Sync {
    fn emit(&self, ev: Event);
}

/// Print each [`Event::transcript_line`] to stdout.
#[derive(Debug, Default)]
pub struct StdoutSink;

impl Sink for StdoutSink {
    fn emit(&self, ev: Event) {
        let line = ev.transcript_line();
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// Append transcript lines to a file; optionally also print to stdout.
#[derive(Debug)]
pub struct FileSink {
    file: Mutex<File>,
    path: PathBuf,
    tee_stdout: bool,
}

impl FileSink {
    /// Create/truncate `path` and append each transcript line (flush after write).
    pub fn create(path: impl AsRef<Path>, tee_stdout: bool) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
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
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Sink for FileSink {
    fn emit(&self, ev: Event) {
        let line = ev.transcript_line();
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
        if self.tee_stdout {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
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
            .map(Event::transcript_line)
            .collect()
    }
}

impl Sink for VecSink {
    fn emit(&self, ev: Event) {
        self.events.lock().expect("vec sink").push(ev);
    }
}
