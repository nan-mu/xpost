//! Decode the firmware's self-describing `get perf` payload into rows.
//!
//! The wire format is described in `xpost-perf`, which the firmware uses to
//! encode. This decoder is a separate implementation because the host build
//! cannot reach that crate across the build staging boundary. The two are kept
//! in step by a test that decodes bytes the firmware encoder produced: if the
//! format moves on one side only, that test fails here instead of a device run
//! producing an unreadable report.
//!
//! # Wire layout
//!
//! ```text
//! magic u32 0x31524650, version u16 (2), reserved u16, count u16
//! entry: name_len u8, name ASCII, kind u8, payload
//!   counter   u64 value
//!   span      u64 count, total_us, min_us, max_us
//!   gauge     u64 current, peak
//!   histogram u64 count, total_us, min_us, max_us, u16 n,
//!             n x (u64 upper_edge, u64 bucket_count)
//!   event     u64 dropped, u32 n, n x (u8 kind, u8 pad[3], u32 arg0, u32 arg1, u32 t_us)
//! ```

use anyhow::{bail, Context, Result};

/// First four bytes of every report.
pub const MAGIC: u32 = 0x3152_4650;
/// Format version this decoder implements.
pub const VERSION: u16 = 2;
/// Bytes before the first entry.
pub const HEADER_LEN: usize = 10;
/// Bytes one event occupies on the wire.
pub const EVENT_WIRE_LEN: usize = 16;
/// Longest entry name.
pub const MAX_NAME: usize = 31;

/// What an entry holds, as the database spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Counter,
    Span,
    Gauge,
    Histogram,
    Event,
}

impl Kind {
    pub fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            1 => Self::Counter,
            2 => Self::Span,
            3 => Self::Gauge,
            4 => Self::Histogram,
            5 => Self::Event,
            other => bail!("unknown report entry kind {other}"),
        })
    }

    /// Name for the `_data.kind` column.
    pub fn text(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Span => "span",
            Self::Gauge => "gauge",
            Self::Histogram => "histogram",
            Self::Event => "event",
        }
    }
}

/// One metric, ready to insert into a `_data` table.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    pub name: String,
    pub kind: Kind,
    pub count: u64,
    pub total_us: u64,
    pub min_us: u64,
    pub max_us: u64,
    pub current: u64,
    pub peak: u64,
    pub edges: Vec<u64>,
    pub buckets: Vec<u64>,
}

impl Metric {
    /// Whether this metric carries no samples.
    ///
    /// The difference between "measured zero" and "never ran" has to reach the
    /// database, or a figure that does not exist becomes a figure of zero.
    pub fn is_absent(&self) -> bool {
        matches!(self.kind, Kind::Span | Kind::Histogram) && self.count == 0
    }
}

/// One timeline event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u8,
    pub t_us: u32,
    pub arg0: u32,
    pub arg1: u32,
}

/// Everything one `get perf` reply says.
#[derive(Clone, Debug, Default)]
pub struct DecodedReport {
    /// Metrics in wire order, which is the firmware plan's order.
    pub metrics: Vec<Metric>,
    /// Events in time order.
    pub events: Vec<Event>,
    /// Ring overflow the firmware reported.
    pub events_dropped: u64,
    /// Entry count the header announced. [`decode_payload`] checks it against
    /// what it parsed, so a caller reading it knows the report was complete.
    pub entries: u16,
}

impl DecodedReport {
    pub fn metric(&self, name: &str) -> Option<&Metric> {
        self.metrics.iter().find(|m| m.name == name)
    }

    pub fn counter(&self, name: &str) -> Option<u64> {
        self.metric(name)
            .filter(|m| m.kind == Kind::Counter)
            .map(|m| m.count)
    }

    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Which section kinds this report carried, in the legacy mask spelling.
    ///
    /// The mask is a description of the report, not of the firmware: a build
    /// that sends no events gets no trace bit, which is what the old
    /// "did this cell have a timeline" check actually meant.
    pub fn mask(&self) -> u8 {
        let mut m = 0u8;
        for metric in &self.metrics {
            m |= match metric.kind {
                Kind::Counter => super::legacy::MASK_C,
                Kind::Span | Kind::Histogram => super::legacy::MASK_A,
                Kind::Gauge => super::legacy::MASK_G,
                Kind::Event => 0,
            };
        }
        if self.has_events() || self.events_dropped != 0 {
            m |= super::legacy::MASK_T;
        }
        m
    }

    /// Trace section as the legacy check expects it, or `None` when the report
    /// carried no events.
    pub fn trace(&self) -> Option<super::legacy::TraceSnap> {
        super::legacy::project(self).trace
    }
}

fn need(buf: &[u8], at: usize, n: usize) -> Result<()> {
    if at.checked_add(n).is_none_or(|e| e > buf.len()) {
        bail!("report truncated at offset {at}");
    }
    Ok(())
}

fn u16_at(buf: &[u8], at: usize) -> Result<u16> {
    need(buf, at, 2)?;
    Ok(u16::from_le_bytes([buf[at], buf[at + 1]]))
}

fn u32_at(buf: &[u8], at: usize) -> Result<u32> {
    need(buf, at, 4)?;
    Ok(u32::from_le_bytes([
        buf[at],
        buf[at + 1],
        buf[at + 2],
        buf[at + 3],
    ]))
}

fn u64_at(buf: &[u8], at: usize) -> Result<u64> {
    need(buf, at, 8)?;
    Ok(u64::from_le_bytes(buf[at..at + 8].try_into().unwrap()))
}

/// Decode the raw payload.
pub fn decode_payload(payload: &[u8]) -> Result<DecodedReport> {
    if payload.len() < HEADER_LEN {
        bail!("report shorter than its header ({} bytes)", payload.len());
    }
    let magic = u32_at(payload, 0)?;
    if magic != MAGIC {
        bail!("not a report: magic {magic:#x}, want {MAGIC:#x}");
    }
    let version = u16_at(payload, 4)?;
    if version != VERSION {
        bail!("report version {version}, this decoder implements {VERSION}");
    }
    let entries = u16_at(payload, 8)?;

    let mut out = DecodedReport {
        entries,
        ..DecodedReport::default()
    };
    let mut at = HEADER_LEN;
    let mut event_entries = 0usize;
    for i in 0..entries {
        let name_len = *payload
            .get(at)
            .with_context(|| format!("entry {i}: truncated at its name length"))?
            as usize;
        at += 1;
        if name_len == 0 || name_len > MAX_NAME {
            bail!("entry {i}: name length {name_len} out of 1..={MAX_NAME}");
        }
        need(payload, at, name_len)?;
        let name = std::str::from_utf8(&payload[at..at + name_len])
            .with_context(|| format!("entry {i}: name is not UTF-8"))?
            .to_string();
        at += name_len;
        let kind_byte = *payload
            .get(at)
            .with_context(|| format!("entry {i} ({name}): truncated at its kind"))?;
        at += 1;
        let kind = Kind::from_u8(kind_byte)
            .with_context(|| format!("entry {i} ({name})"))?;

        match kind {
            Kind::Counter => {
                let value = u64_at(payload, at)?;
                at += 8;
                out.metrics.push(Metric {
                    name,
                    kind,
                    count: value,
                    total_us: 0,
                    min_us: 0,
                    max_us: 0,
                    current: value,
                    peak: 0,
                    edges: Vec::new(),
                    buckets: Vec::new(),
                });
            }
            Kind::Span => {
                let count = u64_at(payload, at)?;
                let total_us = u64_at(payload, at + 8)?;
                let min_us = u64_at(payload, at + 16)?;
                let max_us = u64_at(payload, at + 24)?;
                at += 32;
                out.metrics.push(Metric {
                    name,
                    kind,
                    count,
                    total_us,
                    min_us,
                    max_us,
                    current: 0,
                    peak: 0,
                    edges: Vec::new(),
                    buckets: Vec::new(),
                });
            }
            Kind::Gauge => {
                let current = u64_at(payload, at)?;
                let peak = u64_at(payload, at + 8)?;
                at += 16;
                out.metrics.push(Metric {
                    name,
                    kind,
                    count: 0,
                    total_us: 0,
                    min_us: 0,
                    max_us: 0,
                    current,
                    peak,
                    edges: Vec::new(),
                    buckets: Vec::new(),
                });
            }
            Kind::Histogram => {
                let count = u64_at(payload, at)?;
                let total_us = u64_at(payload, at + 8)?;
                let min_us = u64_at(payload, at + 16)?;
                let max_us = u64_at(payload, at + 24)?;
                let n = u16_at(payload, at + 32)? as usize;
                at += 34;
                let mut edges = Vec::with_capacity(n);
                let mut buckets = Vec::with_capacity(n);
                for _ in 0..n {
                    edges.push(u64_at(payload, at)?);
                    buckets.push(u64_at(payload, at + 8)?);
                    at += 16;
                }
                out.metrics.push(Metric {
                    name,
                    kind,
                    count,
                    total_us,
                    min_us,
                    max_us,
                    current: 0,
                    peak: 0,
                    edges,
                    buckets,
                });
            }
            Kind::Event => {
                event_entries += 1;
                let dropped = u64_at(payload, at)?;
                let n = u32_at(payload, at + 8)? as usize;
                at += 12;
                out.events_dropped = out.events_dropped.saturating_add(dropped);
                need(payload, at, n.checked_mul(EVENT_WIRE_LEN).context("event count overflow")?)?;
                for _ in 0..n {
                    let b = &payload[at..at + EVENT_WIRE_LEN];
                    out.events.push(Event {
                        kind: b[0],
                        arg0: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                        arg1: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
                        t_us: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
                    });
                    at += EVENT_WIRE_LEN;
                }
            }
        }
    }
    if at != payload.len() {
        bail!(
            "report declared {entries} entries but {} trailing bytes remain",
            payload.len() - at
        );
    }
    // The declared count and the parsed parts must agree. A report that says
    // three entries but yields two metrics and no events is a format the two
    // sides disagree about, and silently returning the two would look like a
    // firmware that reports less than it does.
    let parsed = out.metrics.len() + event_entries;
    if parsed != entries as usize {
        bail!("report declared {entries} entries but {parsed} parsed");
    }
    Ok(out)
}

fn decode_b64(s: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let s = s.trim_end_matches('=');
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let a = val(bytes[i]).context("b64")?;
        let b = val(bytes[i + 1]).context("b64")?;
        let c = val(bytes[i + 2]).context("b64")?;
        let d = val(bytes[i + 3]).context("b64")?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
        out.push((c << 6) | d);
        i += 4;
    }
    let rem = bytes.len() - i;
    if rem == 2 {
        let a = val(bytes[i]).context("b64")?;
        let b = val(bytes[i + 1]).context("b64")?;
        out.push((a << 2) | (b >> 4));
    } else if rem == 3 {
        let a = val(bytes[i]).context("b64")?;
        let b = val(bytes[i + 1]).context("b64")?;
        let c = val(bytes[i + 2]).context("b64")?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
    } else if rem != 0 {
        bail!("bad b64 remainder");
    }
    Ok(out)
}

/// Agent `get perf` reply: `OK get perf <n> b64=…`.
pub fn parse_report_b64(msg: &str) -> Result<Vec<u8>> {
    let msg = msg.trim();
    let b64 = msg
        .split_whitespace()
        .find_map(|p| p.strip_prefix("b64="))
        .context("get/perf reply missing b64=")?;
    decode_b64(b64)
}

/// Decode an agent `get perf` reply.
pub fn decode_report_msg(msg: &str) -> Result<DecodedReport> {
    let payload = parse_report_b64(msg)?;
    decode_payload(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-built bytes matching the firmware encoder. Built here, not imported,
    /// because this crate cannot reach `xpost-perf`; the layout above is the
    /// contract both sides follow.
    fn report(metrics: &[(&str, Kind, [u64; 4], &[(u64, u64)])], events: Option<(u64, &[Event])>) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_le_bytes());
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        let count = metrics.len() as u16 + u16::from(events.is_some());
        b.extend_from_slice(&count.to_le_bytes());
        for (name, kind, stats, hist) in metrics {
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
            b.push(match kind {
                Kind::Counter => 1,
                Kind::Span => 2,
                Kind::Gauge => 3,
                Kind::Histogram => 4,
                Kind::Event => 5,
            });
            match kind {
                Kind::Counter => b.extend_from_slice(&stats[0].to_le_bytes()),
                Kind::Span => {
                    for v in stats {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                }
                Kind::Gauge => {
                    b.extend_from_slice(&stats[0].to_le_bytes());
                    b.extend_from_slice(&stats[1].to_le_bytes());
                }
                Kind::Histogram => {
                    for v in stats {
                        b.extend_from_slice(&v.to_le_bytes());
                    }
                    b.extend_from_slice(&(hist.len() as u16).to_le_bytes());
                    for (e, c) in *hist {
                        b.extend_from_slice(&e.to_le_bytes());
                        b.extend_from_slice(&c.to_le_bytes());
                    }
                }
                Kind::Event => unreachable!(),
            }
        }
        if let Some((dropped, evs)) = events {
            b.push("flight".len() as u8);
            b.extend_from_slice(b"flight");
            b.push(5);
            b.extend_from_slice(&dropped.to_le_bytes());
            b.extend_from_slice(&(evs.len() as u32).to_le_bytes());
            for e in evs {
                b.push(e.kind);
                b.extend_from_slice(&[0, 0, 0]);
                b.extend_from_slice(&e.arg0.to_le_bytes());
                b.extend_from_slice(&e.arg1.to_le_bytes());
                b.extend_from_slice(&e.t_us.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn counter_span_gauge_and_histogram_all_decode() {
        let bytes = report(
            &[
                ("calls", Kind::Counter, [7, 0, 0, 0], &[]),
                ("engine_us", Kind::Span, [3, 60, 10, 30], &[]),
                ("depth", Kind::Gauge, [5, 9, 0, 0], &[]),
                ("lookup_us", Kind::Histogram, [3, 60, 1, 40], &[(4, 1), (16, 2)]),
            ],
            None,
        );
        let got = decode_payload(&bytes).unwrap();
        assert_eq!(got.entries, 4);
        assert_eq!(got.counter("calls"), Some(7));
        let span = got.metric("engine_us").unwrap();
        assert_eq!((span.count, span.total_us, span.min_us, span.max_us), (3, 60, 10, 30));
        let gauge = got.metric("depth").unwrap();
        assert_eq!((gauge.current, gauge.peak), (5, 9));
        let hist = got.metric("lookup_us").unwrap();
        assert_eq!(hist.edges, vec![4, 16]);
        assert_eq!(hist.buckets, vec![1, 2]);
    }

    #[test]
    fn an_unsampled_span_is_absent_not_zero() {
        let bytes = report(&[("engine_us", Kind::Span, [0, 0, 0, 0], &[])], None);
        let got = decode_payload(&bytes).unwrap();
        assert!(got.metric("engine_us").unwrap().is_absent());
    }

    #[test]
    fn events_keep_their_order_and_drop_count() {
        let evs = [
            Event { kind: 2, t_us: 100, arg0: 0, arg1: 0 },
            Event { kind: 11, t_us: 101, arg0: 1, arg1: 0 },
            Event { kind: 3, t_us: 120, arg0: 2, arg1: 0 },
        ];
        let bytes = report(&[("calls", Kind::Counter, [1, 0, 0, 0], &[])], Some((4, &evs)));
        let got = decode_payload(&bytes).unwrap();
        assert_eq!(got.events, evs.to_vec());
        assert_eq!(got.events_dropped, 4, "a dropped event is visible, not silent");
    }

    #[test]
    fn a_foreign_report_is_an_error_not_an_empty_one() {
        // An empty report would read as "this firmware reports nothing", which
        // looks far more plausible than a format mismatch.
        let mut bytes = report(&[], None);
        bytes[0] ^= 0xff;
        let err = decode_payload(&bytes).unwrap_err();
        assert!(format!("{err}").contains("magic"), "{err}");
    }

    #[test]
    fn another_version_is_refused_not_decoded_optimistically() {
        let mut bytes = report(&[("calls", Kind::Counter, [1, 0, 0, 0], &[])], None);
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        let err = decode_payload(&bytes).unwrap_err();
        assert!(format!("{err}").contains("version"), "{err}");
    }

    #[test]
    fn a_truncated_report_fails_instead_of_returning_a_short_list() {
        let bytes = report(
            &[
                ("calls", Kind::Counter, [7, 0, 0, 0], &[]),
                ("engine_us", Kind::Span, [3, 60, 10, 30], &[]),
            ],
            None,
        );
        let err = decode_payload(&bytes[..bytes.len() - 8]).unwrap_err();
        assert!(format!("{err}").contains("truncated"), "{err}");
    }

    #[test]
    fn trailing_bytes_are_refused_not_ignored() {
        let mut bytes = report(&[("calls", Kind::Counter, [7, 0, 0, 0], &[])], None);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let err = decode_payload(&bytes).unwrap_err();
        assert!(format!("{err}").contains("trailing"), "{err}");
    }

    #[test]
    fn an_unknown_kind_is_refused() {
        let mut bytes = report(&[("calls", Kind::Counter, [7, 0, 0, 0], &[])], None);
        // Header 10, name length 1 at 10, name 5 bytes, kind byte follows.
        bytes[10 + 1 + 5] = 99;
        let err = decode_payload(&bytes).unwrap_err();
        // The context names the entry, so the sentence carries both the entry
        // and the offending kind byte.
        assert!(format!("{err:#}").contains("unknown report entry kind 99"), "{err:#}");
    }
}
