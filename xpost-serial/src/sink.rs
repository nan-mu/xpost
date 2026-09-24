//! Sync event sink. Implementations must not block for long.

use std::io::{self, Write};
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
