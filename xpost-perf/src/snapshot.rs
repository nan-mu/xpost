//! A Metrics export concatenates an arm snapshot and a freeze snapshot.
//!
//! Each snapshot starts with this 84-byte little-endian header.
//!
//! | Offset | Field |
//! | ---: | --- |
//! | 0 | `NMS1` magic, 4 bytes |
//! | 4 | Version, u16, currently 1 |
//! | 6 | Boundary, u8, 1 for arm or 2 for freeze |
//! | 7 | Tick width, u8 |
//! | 8 | PFR1 byte length, u32 |
//! | 12 | Run UUID, 16 bytes |
//! | 28 | Epoch, u32 |
//! | 32 | Boot ID, u64 |
//! | 40 | Profile SHA-256, 32 bytes |
//! | 72 | Boundary tick, u64 |
//! | 80 | Tick frequency in Hz, u32 |
//!
//! The header precedes one PFR1 v2 report of cumulative u64 counters.

use crate::session::{RunStamp, snapshots_comparable};
use crate::{
    DecodeError, Entry, HEADER_LEN, Kind, SliceSink, SpanStats, Visitor, encode_header,
    encode_metric, visit_entries,
};

pub const SNAPSHOT_HEADER_LEN: usize = 84;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Boundary {
    Arm = 1,
    Freeze = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot<'a> {
    pub boundary: Boundary,
    pub run_uuid: [u8; 16],
    pub stamp: RunStamp,
    pub tick: u64,
    pub tick_hz: u32,
    pub tick_bits: u8,
    pub pfr: &'a [u8],
}

impl<'a> Snapshot<'a> {
    /// Decode one complete snapshot and return any remaining bytes.
    pub fn decode(src: &'a [u8]) -> Option<(Self, &'a [u8])> {
        let (header, body) = src.split_at_checked(SNAPSHOT_HEADER_LEN)?;
        if &header[..6] != b"NMS1\x01\0" {
            return None;
        }
        let boundary = match header[6] {
            1 => Boundary::Arm,
            2 => Boundary::Freeze,
            _ => return None,
        };
        let tick_bits = header[7];
        let tick_hz = u32::from_le_bytes(header[80..84].try_into().ok()?);
        if !(1..=64).contains(&tick_bits) || tick_hz == 0 {
            return None;
        }
        let len = u32::from_le_bytes(header[8..12].try_into().ok()?) as usize;
        let (pfr, rest) = body.split_at_checked(len)?;
        let mut validator = CounterReport { len: HEADER_LEN };
        let report = visit_entries(pfr, &mut validator).ok()?;
        if report.count == 0 || validator.len != len {
            return None;
        }
        Some((
            Self {
                boundary,
                run_uuid: header[12..28].try_into().ok()?,
                stamp: RunStamp {
                    epoch: u32::from_le_bytes(header[28..32].try_into().ok()?),
                    boot_id: u64::from_le_bytes(header[32..40].try_into().ok()?),
                    profile_hash: header[40..72].try_into().ok()?,
                },
                tick: u64::from_le_bytes(header[72..80].try_into().ok()?),
                tick_hz,
                tick_bits,
                pfr,
            },
            rest,
        ))
    }
}

/// Check both boundaries and their common run identity.
pub fn decode_pair(src: &[u8]) -> Option<(Snapshot<'_>, Snapshot<'_>)> {
    let (arm, rest) = Snapshot::decode(src)?;
    let (freeze, rest) = Snapshot::decode(rest)?;
    if !rest.is_empty()
        || arm.boundary != Boundary::Arm
        || freeze.boundary != Boundary::Freeze
        || !snapshots_comparable(&arm.stamp, &freeze.stamp)
        || arm.run_uuid != freeze.run_uuid
        || arm.tick_hz != freeze.tick_hz
        || arm.tick_bits != freeze.tick_bits
        || arm.tick > freeze.tick
    {
        return None;
    }
    Some((arm, freeze))
}

struct CounterReport {
    len: usize,
}

impl Visitor for CounterReport {
    fn entry(&mut self, entry: &Entry<'_>) -> Result<(), DecodeError> {
        if entry.kind != Kind::Counter {
            return Err(DecodeError::BadKind(entry.kind as u8));
        }
        self.len += 2 + entry.name.len() + Kind::Counter.fixed_len();
        Ok(())
    }
}

pub(crate) fn encode<'a>(
    out: &mut [u8],
    snapshot: Snapshot<'_>,
    counters: impl ExactSizeIterator<Item = (&'a str, u64)>,
) -> Option<usize> {
    let count = u16::try_from(counters.len()).ok()?;
    if count == 0 || !(1..=64).contains(&snapshot.tick_bits) || snapshot.tick_hz == 0 {
        return None;
    }
    let (header, output) = out.split_at_mut_checked(SNAPSHOT_HEADER_LEN)?;
    let mut sink = SliceSink::new(output);
    if !encode_header(&mut sink, count) {
        return None;
    }
    for (name, value) in counters {
        if !encode_metric(
            &mut sink,
            name,
            Kind::Counter,
            &SpanStats::new(),
            (value, 0),
            &[],
            &[],
        ) {
            return None;
        }
    }
    let len = u32::try_from(sink.wrote()?).ok()?;
    header[..6].copy_from_slice(b"NMS1\x01\0");
    header[6] = snapshot.boundary as u8;
    header[7] = snapshot.tick_bits;
    header[8..12].copy_from_slice(&len.to_le_bytes());
    header[12..28].copy_from_slice(&snapshot.run_uuid);
    header[28..32].copy_from_slice(&snapshot.stamp.epoch.to_le_bytes());
    header[32..40].copy_from_slice(&snapshot.stamp.boot_id.to_le_bytes());
    header[40..72].copy_from_slice(&snapshot.stamp.profile_hash);
    header[72..80].copy_from_slice(&snapshot.tick.to_le_bytes());
    header[80..84].copy_from_slice(&snapshot.tick_hz.to_le_bytes());
    Some(SNAPSHOT_HEADER_LEN + len as usize)
}
