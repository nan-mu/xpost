//! Round-trip and rejection tests for the report format.
//!
//! The encoder runs on the MCU and the decoder on the host, so the only thing
//! that can keep them in agreement is this file: one side writes, the other
//! reads, and the bytes must come back identical.

use xpost_perf::*;

type Record = ([u8; 32], Kind, SpanStats);

/// Fixed-capacity sink, because the report crate has no allocator.
struct Collect {
    items: [Option<Record>; 8],
    len: usize,
}

impl Collect {
    fn new() -> Self {
        Self {
            items: [None; 8],
            len: 0,
        }
    }
}

impl Visitor for Collect {
    fn entry(&mut self, entry: &Entry<'_>) -> Result<(), DecodeError> {
        if self.len < self.items.len() {
            let mut name = [0u8; 32];
            let n = entry.name.len().min(31);
            name[..n].copy_from_slice(&entry.name.as_bytes()[..n]);
            self.items[self.len] = Some((name, entry.kind, entry.stats));
            self.len += 1;
        }
        Ok(())
    }
}

#[test]
fn span_round_trips() {
    let mut stats = SpanStats::new();
    for us in [10, 20, 30] {
        stats.record(us);
    }

    let mut buf = [0u8; 256];
    let mut o = write_header(&mut buf, 1).unwrap();
    o = write_entry(&mut buf, o, "engine", Kind::Span, &stats, (0, 0), &[]).unwrap();

    let mut sink = Collect::new();
    let header = visit_entries(&buf[..o], &mut sink).unwrap();

    assert_eq!(header.count, 1);
    assert_eq!(sink.len, 1);
    let (name, kind, got) = sink.items[0].unwrap();
    assert_eq!(&name[.."engine".len()], b"engine");
    assert_eq!(kind, Kind::Span);
    assert_eq!(got.count, 3);
    assert_eq!(got.total_us, 60);
    assert_eq!(got.min(), Some(10));
    assert_eq!(got.max_us, 30);
    assert_eq!(got.mean(), Some(20));
}

#[test]
fn an_empty_span_reports_no_samples() {
    let stats = SpanStats::new();
    let mut buf = [0u8; 128];
    let mut o = write_header(&mut buf, 1).unwrap();
    o = write_entry(&mut buf, o, "idle", Kind::Span, &stats, (0, 0), &[]).unwrap();

    let mut sink = Collect::new();
    visit_entries(&buf[..o], &mut sink).unwrap();
    let (_, _, got) = sink.items[0].unwrap();
    // A metric that never ran is absent, not zero. Reporting 0 would make
    // "never measured" look like "instant".
    assert_eq!(got.mean(), None);
    assert_eq!(got.min(), None);
}

#[test]
fn every_kind_round_trips() {
    let mut stats = SpanStats::new();
    stats.record(7);

    let mut buf = [0u8; 512];
    let mut o = write_header(&mut buf, 4).unwrap();
    o = write_entry(&mut buf, o, "count", Kind::Counter, &stats, (42, 0), &[]).unwrap();
    o = write_entry(&mut buf, o, "span", Kind::Span, &stats, (0, 0), &[]).unwrap();
    o = write_entry(&mut buf, o, "gauge", Kind::Gauge, &stats, (5, 9), &[]).unwrap();
    o = write_entry(&mut buf, o, "hist", Kind::Histogram, &stats, (0, 0), &[4, 8, 16]).unwrap();

    let mut sink = Collect::new();
    let header = visit_entries(&buf[..o], &mut sink).unwrap();

    assert_eq!(header.count, 4);
    assert_eq!(sink.len, 4);
    assert_eq!(sink.items[3].unwrap().1, Kind::Histogram);
}

#[test]
fn a_foreign_magic_is_rejected() {
    let mut buf = [0u8; 64];
    write_header(&mut buf, 0).unwrap();
    buf[0] ^= 0xff;
    assert_eq!(read_header(&buf), Err(DecodeError::BadMagic));
}

#[test]
fn an_unknown_version_is_rejected_not_guessed() {
    let mut buf = [0u8; 64];
    write_header(&mut buf, 0).unwrap();
    buf[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        read_header(&buf),
        Err(DecodeError::UnsupportedVersion(2)),
        "a reader must refuse a format it does not implement"
    );
}

#[test]
fn a_truncated_report_is_short_not_empty() {
    let stats = SpanStats::new();
    let mut buf = [0u8; 256];
    let mut o = write_header(&mut buf, 1).unwrap();
    o = write_entry(&mut buf, o, "engine", Kind::Span, &stats, (0, 0), &[]).unwrap();

    // Cut inside the entry. A reader that stopped silently would report zero
    // entries and hide the truncation.
    let mut sink = Collect::new();
    let err = visit_entries(&buf[..o - 4], &mut sink).unwrap_err();
    assert_eq!(err, DecodeError::Short);
}

#[test]
fn an_overlong_name_is_refused_at_write() {
    let stats = SpanStats::new();
    let mut buf = [0u8; 256];
    let o = write_header(&mut buf, 1).unwrap();
    let long = "a".repeat(MAX_NAME + 1);
    assert!(write_entry(&mut buf, o, &long, Kind::Span, &stats, (0, 0), &[]).is_none());
}

#[test]
fn a_short_buffer_is_refused_not_overrun() {
    let stats = SpanStats::new();
    let mut buf = [0u8; 12];
    let o = write_header(&mut buf, 1).unwrap();
    assert!(write_entry(&mut buf, o, "engine", Kind::Span, &stats, (0, 0), &[]).is_none());
}
