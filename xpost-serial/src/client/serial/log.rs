//! File-authoritative serial transcript (timestamped lines).

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Append-only log; [`Log::lines_in_range`] re-reads the file (authoritative).
#[derive(Debug)]
pub struct Log {
    file: Mutex<File>,
    path: PathBuf,
    tee_stdout: bool,
}

impl Log {
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
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_line(&self, line: &str) {
        let ts = rfc3339_millis(SystemTime::now());
        let full = format!("{ts} {line}");
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{full}");
            let _ = f.flush();
        }
        if self.tee_stdout {
            let mut out = io::stdout().lock();
            let _ = writeln!(out, "{full}");
            let _ = out.flush();
        }
    }

    /// Lines whose leading RFC3339 timestamp ∈ `[since, until]` (inclusive).
    pub fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> io::Result<Vec<String>> {
        let file = File::open(&self.path)?;
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
    // `YYYY-MM-DDTHH:MM:SS.mmmZ …`
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn file_roundtrip_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("serial.log");
        let log = Log::create(&path, false).unwrap();
        let t0 = SystemTime::now();
        std::thread::sleep(Duration::from_millis(5));
        log.write_line("[cmd] → ping");
        std::thread::sleep(Duration::from_millis(5));
        let t1 = SystemTime::now();
        std::thread::sleep(Duration::from_millis(5));
        log.write_line("[cmd] ← OK ping");
        let mid = log.lines_in_range(t0, t1).unwrap();
        assert_eq!(mid.len(), 1);
        assert!(mid[0].contains("[cmd] → ping"));
        let all = log
            .lines_in_range(t0, SystemTime::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(all.len(), 2);
    }
}
