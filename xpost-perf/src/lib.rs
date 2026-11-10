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

pub mod plan;
pub mod sweep;

/// First four bytes of every report.
pub const MAGIC: u32 = 0x3152_4650;
/// Current format version.
///
/// Version 2 adds [`Kind::Event`], which carries a whole flight's event stream
/// in one entry. A reader that knows only version 1 must refuse version 2
/// rather than skip the entry: the entry's payload length depends on its kind.
pub const VERSION: u16 = 2;

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
    /// A flight's event stream, in time order.
    Event = 5,
}

/// One timeline event, as the firmware rings store it.
///
/// The zero value is a valid "nothing" filler, so a snapshot of a partially
/// filled ring needs no separate length.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Event {
    /// Event kind byte. Its meaning is the firmware's ring contract, not this
    /// crate's: a reader labels it from that contract, never by guessing.
    pub kind: u8,
    pub arg0: u32,
    pub arg1: u32,
    /// Device clock, microseconds.
    pub t_us: u32,
}

/// Bytes one event occupies on the wire.
pub const EVENT_WIRE_LEN: usize = 16;

impl Kind {
    /// Bytes this kind carries after the name, excluding variable-length tails.
    pub const fn fixed_len(self) -> usize {
        match self {
            Self::Counter => 8,
            Self::Span => 32,
            Self::Gauge => 16,
            // count, total, min, max, then a `u16` bucket count. Each bucket
            // then carries one upper edge and one count; see `encode_histogram`.
            Self::Histogram => 34,
            // dropped, then a `u32` event count.
            Self::Event => 12,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Counter),
            2 => Some(Self::Span),
            3 => Some(Self::Gauge),
            4 => Some(Self::Histogram),
            5 => Some(Self::Event),
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

/// Bytes one entry occupies on the wire.
///
/// `tail` is the variable part: `16 * buckets` for a histogram, `EVENT_WIRE_LEN *
/// count` for an event entry, zero otherwise.
pub fn entry_len(name: &str, kind: Kind, tail: usize) -> usize {
    1 + name.len() + 1 + kind.fixed_len() + tail
}

/// Sink for the encoder.
///
/// One encoder serves both jobs: writing the bytes and measuring them. A caller
/// that needs the length first points it at [`Counting`], then points the same
/// encoder at a [`SliceSink`] or a serial writer. Two separate length and write
/// functions would drift, and a mismatch on the wire cannot be repaired after
/// the header has announced a length.
pub trait Sink {
    /// Append `bytes`. Returns `false` when the sink cannot take them all.
    fn put(&mut self, bytes: &[u8]) -> bool;
}

/// Writes into a fixed buffer. `at` tracks the cursor.
pub struct SliceSink<'a> {
    buf: &'a mut [u8],
    at: usize,
    overflow: bool,
}

impl<'a> SliceSink<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            at: 0,
            overflow: false,
        }
    }

    /// Bytes written, or `None` if anything overflowed.
    pub fn wrote(&self) -> Option<usize> {
        if self.overflow { None } else { Some(self.at) }
    }
}

impl Sink for SliceSink<'_> {
    fn put(&mut self, bytes: &[u8]) -> bool {
        let end = match self.at.checked_add(bytes.len()) {
            Some(e) if e <= self.buf.len() => e,
            _ => {
                self.overflow = true;
                return false;
            }
        };
        self.buf[self.at..end].copy_from_slice(bytes);
        self.at = end;
        true
    }
}

/// Counts bytes and keeps none.
pub struct Counting(usize);

impl Counting {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn len(&self) -> usize {
        self.0
    }

    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl Default for Counting {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for Counting {
    fn put(&mut self, bytes: &[u8]) -> bool {
        self.0 = self.0.saturating_add(bytes.len());
        true
    }
}

fn put_u64(sink: &mut impl Sink, v: u64) -> bool {
    sink.put(&v.to_le_bytes())
}

/// Writes the report header.
pub fn encode_header(sink: &mut impl Sink, count: u16) -> bool {
    let mut hdr = [0u8; HEADER_LEN];
    hdr[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    hdr[4..6].copy_from_slice(&VERSION.to_le_bytes());
    // reserved stays zero; readers must ignore it.
    hdr[8..10].copy_from_slice(&count.to_le_bytes());
    sink.put(&hdr)
}

fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && name.is_ascii()
}

fn encode_name(sink: &mut impl Sink, name: &str, kind: Kind) -> bool {
    if !name_ok(name) {
        return false;
    }
    if !sink.put(&[name.len() as u8]) {
        return false;
    }
    if !sink.put(name.as_bytes()) {
        return false;
    }
    sink.put(&[kind as u8])
}

/// Writes one counter, span, gauge, or histogram entry.
///
/// `value` is the counter value or a gauge's `(current, peak)`. `edges` and
/// `buckets` are empty unless `kind` is [`Kind::Histogram`]; a histogram carries
/// one upper edge and one count per bucket, because edges alone say where the
/// bins are but not how many samples landed in them.
pub fn encode_metric(
    sink: &mut impl Sink,
    name: &str,
    kind: Kind,
    stats: &SpanStats,
    value: (u64, u64),
    edges: &[u64],
    buckets: &[u64],
) -> bool {
    if kind == Kind::Event {
        // Events carry a different payload shape; use `encode_events`.
        return false;
    }
    match kind {
        Kind::Histogram => {
            if edges.is_empty() || edges.len() != buckets.len() || edges.len() > u16::MAX as usize {
                return false;
            }
        }
        _ => {
            if !edges.is_empty() || !buckets.is_empty() {
                return false;
            }
        }
    }
    if !encode_name(sink, name, kind) {
        return false;
    }
    let ok = match kind {
        Kind::Counter => put_u64(sink, value.0),
        Kind::Span => stats_fields(sink, stats),
        Kind::Gauge => put_u64(sink, value.0) && put_u64(sink, value.1),
        Kind::Histogram => {
            if !stats_fields(sink, stats) || !sink.put(&(edges.len() as u16).to_le_bytes()) {
                return false;
            }
            // Interleaved edge/count, not edges then counts: a reader can then
            // label each bucket in one pass, and a truncated stream fails at the
            // bucket it reached instead of after a full edge table.
            for (edge, bucket) in edges.iter().zip(buckets) {
                if !put_u64(sink, *edge) || !put_u64(sink, *bucket) {
                    return false;
                }
            }
            true
        }
        Kind::Event => unreachable!(),
    };
    ok
}

fn stats_fields(sink: &mut impl Sink, stats: &SpanStats) -> bool {
    for v in [
        stats.count,
        stats.total_us,
        stats.min().unwrap_or(0),
        stats.max_us,
    ] {
        if !put_u64(sink, v) {
            return false;
        }
    }
    true
}

/// Writes one event entry: the ring's overflow count, then the events in order.
///
/// `events` is the snapshot the firmware already keeps for its ring. A report
/// built this way needs one stack array the size of the ring, which is what the
/// fixed-slot encoder used too.
pub fn encode_events(sink: &mut impl Sink, name: &str, dropped: u64, events: &[Event]) -> bool {
    if !encode_name(sink, name, Kind::Event) {
        return false;
    }
    if !put_u64(sink, dropped) {
        return false;
    }
    if !sink.put(&(events.len() as u32).to_le_bytes()) {
        return false;
    }
    for ev in events {
        let mut b = [0u8; EVENT_WIRE_LEN];
        b[0] = ev.kind;
        // b[1..4] pad stays zero.
        b[4..8].copy_from_slice(&ev.arg0.to_le_bytes());
        b[8..12].copy_from_slice(&ev.arg1.to_le_bytes());
        b[12..16].copy_from_slice(&ev.t_us.to_le_bytes());
        if !sink.put(&b) {
            return false;
        }
    }
    true
}

/// Bytes one event entry occupies on the wire.
pub fn event_entry_len(name: &str, count: usize) -> usize {
    entry_len(name, Kind::Event, count * EVENT_WIRE_LEN)
}

/// Writes one entry into a fixed buffer at `at`. Returns the new cursor, or
/// `None` on a short buffer. A thin wrapper over [`encode_metric`].
pub fn write_entry(
    buf: &mut [u8],
    at: usize,
    name: &str,
    kind: Kind,
    stats: &SpanStats,
    value: (u64, u64),
    edges: &[u64],
    buckets: &[u64],
) -> Option<usize> {
    let mut sink = SliceSink {
        buf,
        at,
        overflow: false,
    };
    if !encode_metric(&mut sink, name, kind, stats, value, edges, buckets) {
        return None;
    }
    sink.wrote()
}

/// Writes the report header into a fixed buffer.
pub fn write_header(buf: &mut [u8], count: u16) -> Option<usize> {
    let mut sink = SliceSink::new(buf);
    if !encode_header(&mut sink, count) {
        return None;
    }
    sink.wrote()
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

/// One decoded entry, borrowing its name and edges from the report bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub kind: Kind,
    pub stats: SpanStats,
    /// Counter value, or gauge (current, peak).
    pub value: (u64, u64),
    /// Histogram buckets, empty for other kinds. Each bucket is 16 bytes: a
    /// `u64` upper edge, then a `u64` count.
    pub hist: &'a [u8],
    /// Event entries only: the ring's overflow count.
    pub dropped: u64,
    /// Event entries only: the events, packed as `EVENT_WIRE_LEN` bytes each.
    pub events: &'a [u8],
}

impl Entry<'_> {
    /// Read event `i` of an [`Kind::Event`] entry, or `None` when out of range.
    pub fn event(&self, i: usize) -> Option<Event> {
        let at = i.checked_mul(EVENT_WIRE_LEN)?;
        let bytes = self.events.get(at..at + EVENT_WIRE_LEN)?;
        Some(Event {
            kind: bytes[0],
            arg0: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            arg1: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            t_us: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        })
    }

    /// Number of events this entry carries.
    pub fn event_count(&self) -> usize {
        self.events.len() / EVENT_WIRE_LEN
    }

    /// Histogram bucket count, or 0 for other kinds.
    pub fn bucket_len(&self) -> usize {
        self.hist.len() / 16
    }

    /// Upper edge of histogram bucket `i`, or `None` when out of range.
    pub fn edge(&self, i: usize) -> Option<u64> {
        u64_pair(self.hist, i).map(|p| p.0)
    }

    /// Count of samples in histogram bucket `i`, or `None` when out of range.
    pub fn bucket(&self, i: usize) -> Option<u64> {
        u64_pair(self.hist, i).map(|p| p.1)
    }
}

fn u64_pair(buf: &[u8], i: usize) -> Option<(u64, u64)> {
    let at = i.checked_mul(16)?;
    let b = buf.get(at..at + 16)?;
    let a = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    let c = u64::from_le_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]);
    Some((a, c))
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
        let mut hist: &[u8] = &[];
        let mut dropped = 0u64;
        let mut events: &[u8] = &[];
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
                let nbuckets = {
                    let at = o + 32;
                    if at + 2 > buf.len() {
                        return Err(DecodeError::Short);
                    }
                    u16::from_le_bytes([buf[at], buf[at + 1]]) as usize
                };
                o += Kind::Histogram.fixed_len();
                // One edge and one count per bucket, interleaved.
                let pair = nbuckets.checked_mul(16).ok_or(DecodeError::Short)?;
                let end = o.checked_add(pair).ok_or(DecodeError::Short)?;
                if end > buf.len() {
                    return Err(DecodeError::Short);
                }
                hist = &buf[o..end];
                o = end;
            }
            Kind::Event => {
                dropped = u64_at(buf, o)?;
                let count = {
                    let at = o + 8;
                    if at + 4 > buf.len() {
                        return Err(DecodeError::Short);
                    }
                    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize
                };
                o += Kind::Event.fixed_len();
                let ebytes = count
                    .checked_mul(EVENT_WIRE_LEN)
                    .ok_or(DecodeError::Short)?;
                let end = o.checked_add(ebytes).ok_or(DecodeError::Short)?;
                if end > buf.len() {
                    return Err(DecodeError::Short);
                }
                events = &buf[o..end];
                o = end;
            }
        }
        visitor.entry(&Entry {
            name,
            kind,
            stats,
            value,
            hist,
            dropped,
            events,
        })?;
    }
    Ok(header)
}
