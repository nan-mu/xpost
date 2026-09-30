//! Shared logging / formatting helpers for HTTP + ops.

use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn ts() -> String {
    let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return "??:??:??".into();
    };
    let secs = dur.as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}Z")
}

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}.{:01}s", s, d.subsec_millis() / 100)
    }
}

pub fn fmt_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1}MiB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1}KiB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

/// stderr log that never panics on Broken pipe (agent often runs with closed stderr).
pub fn eprint_line(msg: &str) {
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "{msg}");
    let _ = err.flush();
}

pub fn log_line(peer: &str, msg: &str) {
    eprint_line(&format!("[{}] [{peer}] {msg}", ts()));
}

pub fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

pub fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', "\\n");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
