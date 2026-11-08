//! Versioned, self-describing performance report.
//!
//! The older formats encode a fixed number of slots in a fixed order. That works
//! while every firmware arm reports the same metrics, and breaks as soon as one
//! arm wants a metric the others do not have: the slot exists, its name belongs
//! to another arm, and a reader has to know which firmware produced the bytes
//! before it can label them.
//!
//! This format carries the names. A reader labels every entry from the bytes
//! alone.
//!
//! # Wire layout
//!
//! ```text
//! magic     u32 LE   `0x31524650` ("PFR1")
//! version   u16 LE   format version, currently 1
//! reserved  u16 LE   0; readers must ignore
//! count     u16 LE   number of entries
//! entries   count × entry
//! ```
//!
//! Each entry:
//!
//! ```text
//! name_len  u8      1..=31
//! name      name_len bytes, ASCII
//! kind      u8      1 = counter, 2 = span, 3 = gauge, 4 = histogram
//! payload   depends on kind
//! ```
//!
//! Payloads, all little endian:
//!
//! - **counter** — `u64 value`
//! - **span** — `u64 count`, `u64 total_us`, `u64 min_us`, `u64 max_us`
//! - **gauge** — `u64 current`, `u64 peak`
//! - **histogram** — `u64 count`, `u64 total_us`, `u64 min_us`, `u64 max_us`,
//!   `u16 edges_len`, `edges_len × u64 upper_us`
//!
//! A span is the shape every timing metric needs: how many, how long in total,
//! and the extremes. A histogram adds fixed edges so a reader can report
//! quantiles without keeping every sample.
//!
//! # Why a version field
//!
//! The reader must reject a format it does not understand rather than guess.
//! `version` is that gate. Adding an entry kind does not change the version,
//! because an unknown kind can be skipped: its payload length is implied by the
//! kind byte only when the kind is known, so adding one does bump the version.
//!
//! # No allocation
//!
//! The encoder writes straight into a caller-provided buffer through a closure.
//! Nothing here needs a heap, so the same code runs on the MCU and on a host.

#![no_std]

pub mod sweep;

/// First four bytes of every report.
pub const MAGIC: u32 = 0x3152_4650;
/// Current format version.
pub const VERSION: u16 = 1;

/// Longest entry name, in bytes.
pub const MAX_NAME: usize = 31;

/// Bytes a decoder needs before it knows how long the report is.
pub const HEADER_LEN: usize = 10;

/// What an entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// A monotonically increasing tally.
    Counter = 1,
    /// A duration measured many times.
    Span = 2,
    /// A current value with a high-water mark.
    Gauge = 3,
    /// A duration with fixed bucket edges.
    Histogram = 4,
}

impl Kind {
    /// Bytes this kind carries after the name, excluding histogram edges.
    pub const fn fixed_len(self) -> usize {
        match self {
            Self::Counter => 8,
            Self::Span => 32,
            Self::Gauge => 16,
            // count, total, min, max, then a `u16` edge count.
            Self::Histogram => 34,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Counter),
            2 => Some(Self::Span),
            3 => Some(Self::Gauge),
            4 => Some(Self::Histogram),
            _ => None,
        }
    }
}

/// Running span statistics, as the firmware keeps them.
///
/// The min starts at `u64::MAX` so the first sample sets it. `count == 0` means
/// there is no data, and a reader must report the span as absent rather than
/// zero: a metric that never ran is not a zero-duration metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanStats {
    pub count: u64,
    pub total_us: u64,
    pub min_us: u64,
    pub max_us: u64,
}

impl SpanStats {
    /// Empty statistics.
    pub const fn new() -> Self {
        Self {
            count: 0,
            total_us: 0,
            min_us: u64::MAX,
            max_us: 0,
        }
    }

    /// Add one sample.
    pub fn record(&mut self, us: u64) {
        self.count = self.count.saturating_add(1);
        self.total_us = self.total_us.saturating_add(us);
        if us < self.min_us {
            self.min_us = us;
        }
        if us > self.max_us {
            self.max_us = us;
        }
    }

    /// Mean, or `None` when nothing was recorded.
    pub fn mean(&self) -> Option<u64> {
        if self.count == 0 {
            None
        } else {
            Some(self.total_us / self.count)
        }
    }

    /// The smallest sample, or `None` when nothing was recorded.
    pub fn min(&self) -> Option<u64> {
        (self.count > 0).then_some(self.min_us)
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for SpanStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes the report header. Returns bytes written, or `None` on a short buffer.
pub fn write_header(buf: &mut [u8], count: u16) -> Option<usize> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    buf[4..6].copy_from_slice(&VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&0u16.to_le_bytes());
    buf[8..10].copy_from_slice(&count.to_le_bytes());
    Some(HEADER_LEN)
}

/// Bytes one entry occupies on the wire.
pub fn entry_len(name: &str, kind: Kind, edges: usize) -> usize {
    1 + name.len() + 1 + kind.fixed_len() + edges * 8
}

/// Writes one entry at `at`. Returns the new cursor, or `None` on a short buffer.
pub fn write_entry(
    buf: &mut [u8],
    at: usize,
    name: &str,
    kind: Kind,
    stats: &SpanStats,
    value: (u64, u64),
    edges: &[u64],
) -> Option<usize> {
    if name.is_empty() || name.len() > MAX_NAME || !name.is_ascii() {
        return None;
    }
    let total = entry_len(name, kind, edges.len());
    let end = at.checked_add(total)?;
    if end > buf.len() {
        return None;
    }
    let mut o = at;
    buf[o] = name.len() as u8;
    o += 1;
    buf[o..o + name.len()].copy_from_slice(name.as_bytes());
    o += name.len();
    buf[o] = kind as u8;
    o += 1;

    match kind {
        Kind::Counter => {
            buf[o..o + 8].copy_from_slice(&value.0.to_le_bytes());
            o += 8;
        }
        Kind::Span => {
            for v in [stats.count, stats.total_us, stats.min().unwrap_or(0), stats.max_us] {
                buf[o..o + 8].copy_from_slice(&v.to_le_bytes());
                o += 8;
            }
        }
        Kind::Gauge => {
            buf[o..o + 8].copy_from_slice(&value.0.to_le_bytes());
            o += 8;
            buf[o..o + 8].copy_from_slice(&value.1.to_le_bytes());
            o += 8;
        }
        Kind::Histogram => {
            for v in [stats.count, stats.total_us, stats.min().unwrap_or(0), stats.max_us] {
                buf[o..o + 8].copy_from_slice(&v.to_le_bytes());
                o += 8;
            }
            buf[o..o + 2].copy_from_slice(&(edges.len() as u16).to_le_bytes());
            o += 2;
            for e in edges {
                buf[o..o + 8].copy_from_slice(&e.to_le_bytes());
                o += 8;
            }
        }
    }
    debug_assert_eq!(o, end);
    Some(end)
}

/// Header fields of a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub count: u16,
}

/// Failure modes a reader must distinguish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Fewer bytes than a header.
    ShortHeader,
    /// Not this format.
    BadMagic,
    /// A version this code does not implement.
    UnsupportedVersion(u16),
    /// Truncated inside an entry.
    Short,
    /// A name that cannot be a metric name.
    BadName,
    /// A kind byte this code does not implement.
    BadKind(u8),
}

/// Reads the header.
pub fn read_header(buf: &[u8]) -> Result<Header, DecodeError> {
    if buf.len() < HEADER_LEN {
        return Err(DecodeError::ShortHeader);
    }
    let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if magic != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = u16::from_le_bytes([buf[4], buf[5]]);
    if version != VERSION {
        return Err(DecodeError::UnsupportedVersion(version));
    }
    Ok(Header {
        version,
        count: u16::from_le_bytes([buf[8], buf[9]]),
    })
}

fn u64_at(buf: &[u8], o: usize) -> Result<u64, DecodeError> {
    let end = o.checked_add(8).ok_or(DecodeError::Short)?;
    if end > buf.len() {
        return Err(DecodeError::Short);
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[o..end]);
    Ok(u64::from_le_bytes(b))
}

/// One decoded entry, borrowing its name from the report bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub kind: Kind,
    pub stats: SpanStats,
    /// Counter value, or gauge (current, peak).
    pub value: (u64, u64),
    /// Histogram edges, empty for other kinds.
    pub edges: &'a [u8],
}

/// Visitor for [`visit_entries`].
pub trait Visitor {
    /// Receives each entry in wire order.
    fn entry(&mut self, entry: &Entry<'_>) -> Result<(), DecodeError>;
}

/// Read every entry without allocating.
///
/// Calls `visitor` once per entry. Stops at the first malformed entry so a
/// truncated report cannot be mistaken for a short one.
pub fn visit_entries<V: Visitor>(buf: &[u8], visitor: &mut V) -> Result<Header, DecodeError> {
    let header = read_header(buf)?;
    let mut o = HEADER_LEN;
    for _ in 0..header.count {
        if o >= buf.len() {
            return Err(DecodeError::Short);
        }
        let name_len = buf[o] as usize;
        if name_len == 0 || name_len > MAX_NAME {
            return Err(DecodeError::BadName);
        }
        o += 1;
        let name_end = o.checked_add(name_len).ok_or(DecodeError::Short)?;
        if name_end > buf.len() {
            return Err(DecodeError::Short);
        }
        let name = core::str::from_utf8(&buf[o..name_end]).map_err(|_| DecodeError::BadName)?;
        if !name.is_ascii() {
            return Err(DecodeError::BadName);
        }
        o = name_end;
        if o >= buf.len() {
            return Err(DecodeError::Short);
        }
        let kind = Kind::from_u8(buf[o]).ok_or(DecodeError::BadKind(buf[o]))?;
        o += 1;

        let mut stats = SpanStats::new();
        let mut value = (0u64, 0u64);
        let mut edges: &[u8] = &[];
        match kind {
            Kind::Counter => {
                value.0 = u64_at(buf, o)?;
                o += 8;
            }
            Kind::Span => {
                stats.count = u64_at(buf, o)?;
                stats.total_us = u64_at(buf, o + 8)?;
                stats.min_us = u64_at(buf, o + 16)?;
                stats.max_us = u64_at(buf, o + 24)?;
                if stats.count == 0 {
                    stats.min_us = u64::MAX;
                }
                o += 32;
            }
            Kind::Gauge => {
                value.0 = u64_at(buf, o)?;
                value.1 = u64_at(buf, o + 8)?;
                o += 16;
            }
            Kind::Histogram => {
                stats.count = u64_at(buf, o)?;
                stats.total_us = u64_at(buf, o + 8)?;
                stats.min_us = u64_at(buf, o + 16)?;
                stats.max_us = u64_at(buf, o + 24)?;
                if stats.count == 0 {
                    stats.min_us = u64::MAX;
                }
                let edges_len = {
                    let at = o + 32;
                    if at + 2 > buf.len() {
                        return Err(DecodeError::Short);
                    }
                    u16::from_le_bytes([buf[at], buf[at + 1]]) as usize
                };
                o += Kind::Histogram.fixed_len();
                let ebytes = edges_len * 8;
                let end = o.checked_add(ebytes).ok_or(DecodeError::Short)?;
                if end > buf.len() {
                    return Err(DecodeError::Short);
                }
                edges = &buf[o..end];
                o = end;
            }
        }
        visitor.entry(&Entry {
            name,
            kind,
            stats,
            value,
            edges,
        })?;
    }
    Ok(header)
}
