//! 40-byte trace events for one armed run.
//!
//! The buffer keeps the earliest events. A later event that does not fit is
//! counted in `dropped` and does not overwrite the prefix. Freeze never inserts
//! an end event for a span that is still open.

/// Bytes in one [`TraceEvent`] on the wire.
pub const TRACE_EVENT_LEN: usize = 40;

/// Initial event capacity. A firmware build must account for the bytes before
/// raising this.
pub const TRACE_CAPACITY: usize = 512;

/// `probe_seq` value that means the event could not be attributed.
pub const PROBE_UNATTRIBUTED: u64 = u64::MAX;

/// The buffer stopped accepting events.
pub const FLAG_OVERFLOW: u32 = 1;
/// Freeze saw a begin with no matching end. No end event was invented.
pub const FLAG_INCOMPLETE: u32 = 2;

pub const TRACE_HEADER_LEN: usize = 57;

/// What kind of timeline mark an event is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TraceKind {
    Begin = 1,
    End = 2,
    Instant = 3,
}

impl TraceKind {
    const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Begin),
            2 => Some(Self::End),
            3 => Some(Self::Instant),
            _ => None,
        }
    }
}

/// One trace event. The field order is the little-endian wire order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceEvent {
    pub sequence: u32,
    pub event_id: u16,
    pub kind: TraceKind,
    pub context_id: u8,
    pub tick: u64,
    pub probe_seq: u64,
    pub span_id: u32,
    pub parent_span_id: u32,
    pub arg0: u32,
    pub arg1: u32,
}

impl TraceEvent {
    /// Probe sequence, or `None` when the event is not attributed.
    pub const fn attributed_probe(self) -> Option<u64> {
        if self.probe_seq == PROBE_UNATTRIBUTED {
            None
        } else {
            Some(self.probe_seq)
        }
    }

    pub fn encode(self, out: &mut [u8]) -> bool {
        if out.len() < TRACE_EVENT_LEN {
            return false;
        }
        out[..4].copy_from_slice(&self.sequence.to_le_bytes());
        out[4..6].copy_from_slice(&self.event_id.to_le_bytes());
        out[6] = self.kind as u8;
        out[7] = self.context_id;
        out[8..16].copy_from_slice(&self.tick.to_le_bytes());
        out[16..24].copy_from_slice(&self.probe_seq.to_le_bytes());
        out[24..28].copy_from_slice(&self.span_id.to_le_bytes());
        out[28..32].copy_from_slice(&self.parent_span_id.to_le_bytes());
        out[32..36].copy_from_slice(&self.arg0.to_le_bytes());
        out[36..40].copy_from_slice(&self.arg1.to_le_bytes());
        true
    }

    pub fn decode(src: &[u8]) -> Option<Self> {
        if src.len() < TRACE_EVENT_LEN {
            return None;
        }
        Some(Self {
            sequence: u32::from_le_bytes(src[0..4].try_into().ok()?),
            event_id: u16::from_le_bytes(src[4..6].try_into().ok()?),
            kind: TraceKind::from_u8(src[6])?,
            context_id: src[7],
            tick: u64::from_le_bytes(src[8..16].try_into().ok()?),
            probe_seq: u64::from_le_bytes(src[16..24].try_into().ok()?),
            span_id: u32::from_le_bytes(src[24..28].try_into().ok()?),
            parent_span_id: u32::from_le_bytes(src[28..32].try_into().ok()?),
            arg0: u32::from_le_bytes(src[32..36].try_into().ok()?),
            arg1: u32::from_le_bytes(src[36..40].try_into().ok()?),
        })
    }
}

/// Fixed prefix of a trace run, little-endian, 57 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceHeader {
    pub format_version: u16,
    pub dictionary_version: u16,
    pub run_uuid: [u8; 16],
    pub epoch: u32,
    pub boot_id: u64,
    pub tick_hz: u64,
    pub tick_bits: u8,
    pub capacity: u32,
    pub written: u32,
    pub dropped: u32,
    pub flags: u32,
}

impl TraceHeader {
    pub fn encode(self, out: &mut [u8]) -> bool {
        if out.len() < TRACE_HEADER_LEN {
            return false;
        }
        out[0..2].copy_from_slice(&self.format_version.to_le_bytes());
        out[2..4].copy_from_slice(&self.dictionary_version.to_le_bytes());
        out[4..20].copy_from_slice(&self.run_uuid);
        out[20..24].copy_from_slice(&self.epoch.to_le_bytes());
        out[24..32].copy_from_slice(&self.boot_id.to_le_bytes());
        out[32..40].copy_from_slice(&self.tick_hz.to_le_bytes());
        out[40] = self.tick_bits;
        out[41..45].copy_from_slice(&self.capacity.to_le_bytes());
        out[45..49].copy_from_slice(&self.written.to_le_bytes());
        out[49..53].copy_from_slice(&self.dropped.to_le_bytes());
        out[53..57].copy_from_slice(&self.flags.to_le_bytes());
        true
    }

    pub fn decode(src: &[u8]) -> Option<Self> {
        if src.len() < TRACE_HEADER_LEN {
            return None;
        }
        let mut run_uuid = [0u8; 16];
        run_uuid.copy_from_slice(&src[4..20]);
        Some(Self {
            format_version: u16::from_le_bytes(src[0..2].try_into().ok()?),
            dictionary_version: u16::from_le_bytes(src[2..4].try_into().ok()?),
            run_uuid,
            epoch: u32::from_le_bytes(src[20..24].try_into().ok()?),
            boot_id: u64::from_le_bytes(src[24..32].try_into().ok()?),
            tick_hz: u64::from_le_bytes(src[32..40].try_into().ok()?),
            tick_bits: src[40],
            capacity: u32::from_le_bytes(src[41..45].try_into().ok()?),
            written: u32::from_le_bytes(src[45..49].try_into().ok()?),
            dropped: u32::from_le_bytes(src[49..53].try_into().ok()?),
            flags: u32::from_le_bytes(src[53..57].try_into().ok()?),
        })
    }
}

/// In-memory trace that refuses to overwrite its prefix.
#[derive(Clone, Debug)]
pub struct TraceLog<const N: usize> {
    events: [Option<TraceEvent>; N],
    written: u32,
    dropped: u32,
    flags: u32,
}

impl<const N: usize> TraceLog<N> {
    pub const fn new() -> Self {
        Self {
            events: [None; N],
            written: 0,
            dropped: 0,
            flags: 0,
        }
    }

    pub const fn written(&self) -> u32 {
        self.written
    }

    pub const fn dropped(&self) -> u32 {
        self.dropped
    }

    pub const fn flags(&self) -> u32 {
        self.flags
    }

    pub fn get(&self, index: usize) -> Option<TraceEvent> {
        self.events.get(index).copied().flatten()
    }

    /// Record one event, or count a drop when the prefix is already full.
    pub fn push(&mut self, event: TraceEvent) {
        let index = self.written as usize;
        if index >= N {
            self.dropped = self.dropped.saturating_add(1);
            self.flags |= FLAG_OVERFLOW;
            return;
        }
        self.events[index] = Some(event);
        self.written += 1;
    }

    /// Mark an open span. Does not append an end event.
    pub fn freeze(&mut self) {
        let mut open = [0u32; N];
        let mut depth = 0usize;
        for index in 0..self.written as usize {
            let Some(event) = self.events[index] else {
                continue;
            };
            match event.kind {
                TraceKind::Begin => {
                    if depth < N {
                        open[depth] = event.span_id;
                        depth += 1;
                    }
                }
                TraceKind::End => {
                    if let Some(pos) = open[..depth].iter().rposition(|id| *id == event.span_id) {
                        open.copy_within(pos + 1..depth, pos);
                        depth -= 1;
                    }
                }
                TraceKind::Instant => {}
            }
        }
        if depth > 0 {
            self.flags |= FLAG_INCOMPLETE;
        }
    }
}

const _: () = assert!(TRACE_EVENT_LEN == 40);
const _: () = assert!(TRACE_CAPACITY == 512);

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(kind: TraceKind, span: u32) -> TraceEvent {
        TraceEvent {
            sequence: 7,
            event_id: 11,
            kind,
            context_id: 2,
            tick: 99,
            probe_seq: PROBE_UNATTRIBUTED,
            span_id: span,
            parent_span_id: 0,
            arg0: 1,
            arg1: 2,
        }
    }

    #[test]
    fn event_round_trip_keeps_unattributed_probe() {
        let event = sample(TraceKind::Instant, 4);
        let mut buf = [0u8; TRACE_EVENT_LEN];
        assert!(event.encode(&mut buf));
        let decoded = TraceEvent::decode(&buf).unwrap();
        assert_eq!(decoded, event);
        assert_eq!(decoded.attributed_probe(), None);
        assert!(TraceEvent::decode(&buf[..39]).is_none());
    }

    #[test]
    fn a_full_log_keeps_the_prefix() {
        let mut log = TraceLog::<2>::new();
        log.push(sample(TraceKind::Instant, 1));
        log.push(sample(TraceKind::Instant, 2));
        log.push(sample(TraceKind::Instant, 3));
        assert_eq!(log.written(), 2);
        assert_eq!(log.dropped(), 1);
        assert_eq!(log.flags() & FLAG_OVERFLOW, FLAG_OVERFLOW);
        assert_eq!(log.get(0).unwrap().span_id, 1);
        assert_eq!(log.get(1).unwrap().span_id, 2);
        assert!(log.get(2).is_none());
    }

    #[test]
    fn freeze_marks_an_open_span_and_does_not_invent_an_end() {
        let mut log = TraceLog::<4>::new();
        log.push(sample(TraceKind::Begin, 8));
        log.freeze();
        assert_eq!(log.written(), 1);
        assert_eq!(log.flags() & FLAG_INCOMPLETE, FLAG_INCOMPLETE);
        assert_eq!(log.get(0).unwrap().kind, TraceKind::Begin);
    }

    #[test]
    fn a_closed_span_is_complete() {
        let mut log = TraceLog::<4>::new();
        log.push(sample(TraceKind::Begin, 8));
        log.push(sample(TraceKind::End, 8));
        log.freeze();
        assert_eq!(log.flags() & FLAG_INCOMPLETE, 0);
        assert_eq!(log.written(), 2);
    }

    #[test]
    fn header_round_trip() {
        let header = TraceHeader {
            format_version: 1,
            dictionary_version: 1,
            run_uuid: [9; 16],
            epoch: 3,
            boot_id: 4,
            tick_hz: 16_000_000,
            tick_bits: 54,
            capacity: TRACE_CAPACITY as u32,
            written: 2,
            dropped: 5,
            flags: FLAG_OVERFLOW,
        };
        let mut buf = [0u8; TRACE_HEADER_LEN];
        assert!(header.encode(&mut buf));
        assert_eq!(TraceHeader::decode(&buf), Some(header));
    }
}
