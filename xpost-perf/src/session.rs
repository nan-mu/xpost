//! One measurement run on the existing perf path.
//!
//! `prepare` installs an epoch. `arm` opens recording. `freeze` closes it and
//! holds the export until `release`. A second `prepare` while that export is
//! held returns [`PerfError::Busy`]. A command whose epoch is not the
//! installed one is rejected before any byte is copied or any event is stored.
//! An end event from an older epoch therefore cannot close a span in the new
//! one.

use crate::snapshot::{self, Boundary, Snapshot};
use crate::trace::{TRACE_EVENT_LEN, TRACE_HEADER_LEN, TraceEvent, TraceHeader, TraceLog};

/// Raw SHA-256 bytes. The database stores the same value as 64 hex characters.
pub const PROFILE_HASH_LEN: usize = 32;

/// Largest `perf read` chunk. A larger request is cut down to this.
pub const READ_CHUNK_MAX: usize = 512;

const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;
const FORMAT_VERSION: u16 = 1;

/// Which capture this run stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Metrics,
    Trace,
}

/// Why a perf command was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfError {
    /// An export still owns the buffer, or a run is already open.
    Busy,
    /// The epoch is not the one `prepare` installed.
    WrongEpoch,
    /// The command does not belong to the current phase.
    WrongPhase,
    /// `offset` is past the end of the export.
    OffsetOutOfRange,
    /// The epoch counter cannot advance without wrapping.
    EpochExhausted,
    /// The firmware does not have storage for the requested variant.
    Unsupported,
    /// A complete counter snapshot could not be encoded.
    InvalidSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Prepared,
    Armed,
    Frozen,
}

/// Identity a host checks before subtracting two snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunStamp {
    pub boot_id: u64,
    pub epoch: u32,
    pub profile_hash: [u8; PROFILE_HASH_LEN],
}

/// `true` only when both snapshots are the same boot, epoch, and profile.
pub fn snapshots_comparable(left: &RunStamp, right: &RunStamp) -> bool {
    left.boot_id == right.boot_id
        && left.epoch == right.epoch
        && left.profile_hash == right.profile_hash
}

/// Bytes returned by one `read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadChunk {
    pub copied: usize,
    pub total: u32,
    pub digest: u32,
}

/// Bytes and flags returned by `freeze`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreezeReport {
    pub freeze_tick: u64,
    pub flags: u32,
    pub len: u32,
    pub digest: u32,
}

/// The run currently installed on one device.
#[derive(Clone, Debug)]
pub struct PerfSession<const N: usize, const M: usize = 0> {
    phase: Phase,
    epoch: u32,
    boot_id: u64,
    tick_hz: u64,
    tick_bits: u8,
    dictionary_version: u16,
    run_uuid: [u8; 16],
    profile_hash: [u8; PROFILE_HASH_LEN],
    variant: Variant,
    arm_tick: u64,
    log: TraceLog<N>,
    metrics: [u8; M],
    metrics_len: usize,
    frozen_digest: u32,
}

impl<const N: usize, const M: usize> PerfSession<N, M> {
    pub const fn new(boot_id: u64, tick_hz: u64, tick_bits: u8, dictionary_version: u16) -> Self {
        Self {
            phase: Phase::Idle,
            epoch: 0,
            boot_id,
            tick_hz,
            tick_bits,
            dictionary_version,
            run_uuid: [0; 16],
            profile_hash: [0; PROFILE_HASH_LEN],
            variant: Variant::Metrics,
            arm_tick: 0,
            log: TraceLog::new(),
            metrics: [0; M],
            metrics_len: 0,
            frozen_digest: 0,
        }
    }

    pub const fn epoch(&self) -> u32 {
        self.epoch
    }

    pub const fn boot_id(&self) -> u64 {
        self.boot_id
    }

    /// Set a boot identity before the first prepare.
    pub fn set_boot_id(&mut self, boot_id: u64) -> Result<(), PerfError> {
        if self.epoch != 0 || self.boot_id != 0 || boot_id == 0 {
            return Err(PerfError::WrongPhase);
        }
        self.boot_id = boot_id;
        Ok(())
    }

    /// The current Trace identity while recording.
    pub fn active_trace(&self) -> Option<(u32, &[u8; 16])> {
        (self.phase == Phase::Armed && self.variant == Variant::Trace)
            .then_some((self.epoch, &self.run_uuid))
    }

    /// The current run identity while either variant records.
    pub fn active_run(&self) -> Option<(u32, &[u8; 16])> {
        (self.phase == Phase::Armed).then_some((self.epoch, &self.run_uuid))
    }

    pub const fn variant(&self) -> Variant {
        self.variant
    }

    /// Stamp of the open run. `None` after `release` and before the next `prepare`.
    pub fn stamp(&self) -> Option<RunStamp> {
        if self.phase == Phase::Idle {
            None
        } else {
            Some(RunStamp {
                boot_id: self.boot_id,
                epoch: self.epoch,
                profile_hash: self.profile_hash,
            })
        }
    }

    /// Install a new epoch. Refused while a run or its export still owns the buffer.
    pub fn prepare(
        &mut self,
        run_uuid: [u8; 16],
        variant: Variant,
        profile_hash: [u8; PROFILE_HASH_LEN],
    ) -> Result<(u32, u64), PerfError> {
        if self.phase != Phase::Idle {
            return Err(PerfError::Busy);
        }
        if (variant == Variant::Metrics && M == 0) || (variant == Variant::Trace && N == 0) {
            return Err(PerfError::Unsupported);
        }
        let epoch = self.epoch.checked_add(1).ok_or(PerfError::EpochExhausted)?;
        self.epoch = epoch;
        self.run_uuid = run_uuid;
        self.profile_hash = profile_hash;
        self.variant = variant;
        self.arm_tick = 0;
        self.log = TraceLog::new();
        self.metrics_len = 0;
        self.phase = Phase::Prepared;
        Ok((epoch, self.boot_id))
    }

    /// Open Trace recording for `epoch`.
    pub fn arm(&mut self, epoch: u32, tick: u64) -> Result<u64, PerfError> {
        self.gate(Phase::Prepared, epoch)?;
        if self.variant != Variant::Trace {
            return Err(PerfError::WrongPhase);
        }
        Ok(self.open(tick))
    }

    /// Capture the Metrics baseline and open the window.
    pub fn arm_metrics<'a>(
        &mut self,
        epoch: u32,
        tick: u64,
        counters: impl ExactSizeIterator<Item = (&'a str, u64)>,
    ) -> Result<u64, PerfError> {
        self.gate(Phase::Prepared, epoch)?;
        self.capture_metrics(Boundary::Arm, tick, counters)?;
        Ok(self.open(tick))
    }

    fn open(&mut self, tick: u64) -> u64 {
        self.arm_tick = tick;
        self.phase = Phase::Armed;
        tick
    }

    /// Store one trace event. A mismatched epoch stores nothing.
    pub fn push_event(&mut self, epoch: u32, event: TraceEvent) -> Result<(), PerfError> {
        self.gate(Phase::Armed, epoch)?;
        if self.variant != Variant::Trace {
            return Err(PerfError::WrongPhase);
        }
        self.log.push(event);
        Ok(())
    }

    /// Close Trace recording and hold the export until `release`.
    pub fn freeze(&mut self, epoch: u32, tick: u64) -> Result<FreezeReport, PerfError> {
        self.gate(Phase::Armed, epoch)?;
        if self.variant != Variant::Trace {
            return Err(PerfError::WrongPhase);
        }
        self.log.freeze();
        Ok(self.close(tick))
    }

    /// Capture the final Metrics counters and hold both snapshots.
    pub fn freeze_metrics<'a>(
        &mut self,
        epoch: u32,
        tick: u64,
        counters: impl ExactSizeIterator<Item = (&'a str, u64)>,
    ) -> Result<FreezeReport, PerfError> {
        self.gate(Phase::Armed, epoch)?;
        if tick < self.arm_tick {
            return Err(PerfError::InvalidSnapshot);
        }
        self.capture_metrics(Boundary::Freeze, tick, counters)?;
        Ok(self.close(tick))
    }

    fn capture_metrics<'a>(
        &mut self,
        boundary: Boundary,
        tick: u64,
        counters: impl ExactSizeIterator<Item = (&'a str, u64)>,
    ) -> Result<(), PerfError> {
        if self.variant != Variant::Metrics {
            return Err(PerfError::WrongPhase);
        }
        let snapshot = Snapshot {
            boundary,
            run_uuid: self.run_uuid,
            stamp: self.stamp().expect("a Metrics epoch is installed"),
            tick,
            tick_hz: u32::try_from(self.tick_hz).map_err(|_| PerfError::InvalidSnapshot)?,
            tick_bits: self.tick_bits,
            pfr: &[],
        };
        let len = snapshot::encode(&mut self.metrics[self.metrics_len..], snapshot, counters)
            .ok_or(PerfError::InvalidSnapshot)?;
        self.metrics_len += len;
        Ok(())
    }

    fn close(&mut self, tick: u64) -> FreezeReport {
        self.phase = Phase::Frozen;
        self.frozen_digest = self.digest();
        FreezeReport {
            freeze_tick: tick,
            flags: self.log.flags(),
            len: self.export_len(),
            digest: self.frozen_digest,
        }
    }

    /// Copy one chunk of the frozen export. The same offset returns the same bytes.
    ///
    /// On error, `out` is left untouched.
    pub fn read(
        &self,
        epoch: u32,
        offset: u32,
        max_bytes: usize,
        out: &mut [u8],
    ) -> Result<ReadChunk, PerfError> {
        self.gate(Phase::Frozen, epoch)?;
        let total = self.export_len();
        if offset > total {
            return Err(PerfError::OffsetOutOfRange);
        }
        let room = (total - offset) as usize;
        let copied = max_bytes.min(READ_CHUNK_MAX).min(out.len()).min(room);
        if copied > 0 {
            self.copy_export(offset, &mut out[..copied]);
        }
        Ok(ReadChunk {
            copied,
            total,
            digest: self.frozen_digest,
        })
    }

    /// Drop the export so a later `prepare` may reuse the buffer.
    pub fn release(&mut self, epoch: u32) -> Result<(), PerfError> {
        self.gate(Phase::Frozen, epoch)?;
        self.log = TraceLog::new();
        self.phase = Phase::Idle;
        Ok(())
    }

    fn gate(&self, want: Phase, epoch: u32) -> Result<(), PerfError> {
        if self.phase == Phase::Idle {
            return Err(PerfError::WrongPhase);
        }
        if epoch != self.epoch {
            return Err(PerfError::WrongEpoch);
        }
        if self.phase != want {
            return Err(PerfError::WrongPhase);
        }
        Ok(())
    }

    fn export_len(&self) -> u32 {
        match self.variant {
            Variant::Metrics => self.metrics_len as u32,
            Variant::Trace => {
                (TRACE_HEADER_LEN + self.log.written() as usize * TRACE_EVENT_LEN) as u32
            }
        }
    }

    fn header_bytes(&self) -> [u8; TRACE_HEADER_LEN] {
        let header = TraceHeader {
            format_version: FORMAT_VERSION,
            dictionary_version: self.dictionary_version,
            run_uuid: self.run_uuid,
            epoch: self.epoch,
            boot_id: self.boot_id,
            tick_hz: self.tick_hz,
            tick_bits: self.tick_bits,
            capacity: N as u32,
            written: self.log.written(),
            dropped: self.log.dropped(),
            flags: self.log.flags(),
        };
        let mut buf = [0u8; TRACE_HEADER_LEN];
        assert!(header.encode(&mut buf));
        buf
    }

    fn digest(&self) -> u32 {
        let mut hash = FNV_OFFSET;
        if self.variant == Variant::Metrics {
            return self.metrics[..self.metrics_len]
                .iter()
                .copied()
                .fold(hash, fnv_byte);
        }
        for byte in self.header_bytes() {
            hash = fnv_byte(hash, byte);
        }
        let mut event_buf = [0u8; TRACE_EVENT_LEN];
        for index in 0..self.log.written() as usize {
            let event = self.log.get(index).expect("written prefix is filled");
            assert!(event.encode(&mut event_buf));
            for byte in event_buf {
                hash = fnv_byte(hash, byte);
            }
        }
        hash
    }

    fn copy_export(&self, offset: u32, out: &mut [u8]) {
        if self.variant == Variant::Metrics {
            let offset = offset as usize;
            out.copy_from_slice(&self.metrics[offset..offset + out.len()]);
            return;
        }
        let header = self.header_bytes();
        let mut filled = 0usize;
        let offset = offset as usize;
        if offset < TRACE_HEADER_LEN {
            let take = (TRACE_HEADER_LEN - offset).min(out.len());
            out[..take].copy_from_slice(&header[offset..offset + take]);
            filled = take;
        }
        let mut pos = if offset < TRACE_HEADER_LEN {
            0
        } else {
            offset - TRACE_HEADER_LEN
        };
        let mut event_buf = [0u8; TRACE_EVENT_LEN];
        while filled < out.len() {
            let index = pos / TRACE_EVENT_LEN;
            let skip = pos % TRACE_EVENT_LEN;
            let event = self.log.get(index).expect("read stays inside the export");
            assert!(event.encode(&mut event_buf));
            let take = (TRACE_EVENT_LEN - skip).min(out.len() - filled);
            out[filled..filled + take].copy_from_slice(&event_buf[skip..skip + take]);
            filled += take;
            pos += take;
        }
    }
}

fn fnv_byte(hash: u32, byte: u8) -> u32 {
    (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{FLAG_INCOMPLETE, TraceKind};

    fn session() -> PerfSession<16> {
        PerfSession::new(0xB007, 16_000_000, 54, 1)
    }

    fn event(kind: TraceKind, span: u32, seq: u32) -> TraceEvent {
        TraceEvent {
            sequence: seq,
            event_id: 11,
            kind,
            context_id: 1,
            tick: u64::from(seq) * 10,
            probe_seq: 4,
            span_id: span,
            parent_span_id: 0,
            arg0: 0,
            arg1: 0,
        }
    }

    fn arm_trace(session: &mut PerfSession<16>, epoch_hint: u32) -> u32 {
        let (epoch, boot) = session
            .prepare(
                [9; 16],
                Variant::Trace,
                [epoch_hint as u8; PROFILE_HASH_LEN],
            )
            .unwrap();
        assert_eq!(boot, 0xB007);
        assert_eq!(session.arm(epoch, 100).unwrap(), 100);
        epoch
    }

    #[test]
    fn a_wrong_epoch_read_does_not_copy_bytes() {
        let mut session = session();
        let epoch = arm_trace(&mut session, 1);
        session
            .push_event(epoch, event(TraceKind::Instant, 1, 1))
            .unwrap();
        session.freeze(epoch, 200).unwrap();
        let mut out = [0xA5u8; 32];
        let err = session.read(epoch.wrapping_add(9), 0, 32, &mut out);
        assert_eq!(err, Err(PerfError::WrongEpoch));
        assert_eq!(out, [0xA5u8; 32]);
    }

    #[test]
    fn prepare_is_busy_until_release_and_then_advances_the_epoch() {
        let mut session = session();
        let epoch = arm_trace(&mut session, 1);
        session.freeze(epoch, 200).unwrap();
        assert_eq!(
            session.prepare([1; 16], Variant::Metrics, [2; PROFILE_HASH_LEN]),
            Err(PerfError::Busy)
        );
        session.release(epoch).unwrap();
        let (next, _) = session
            .prepare([1; 16], Variant::Trace, [2; PROFILE_HASH_LEN])
            .unwrap();
        assert_eq!(next, epoch + 1);
    }

    #[test]
    fn the_same_chunk_reads_back_twice_and_a_past_end_offset_is_refused() {
        let mut session = session();
        let epoch = arm_trace(&mut session, 3);
        for n in 0..13 {
            session
                .push_event(epoch, event(TraceKind::Instant, 1, n))
                .unwrap();
        }
        let report = session.freeze(epoch, 900).unwrap();
        assert!(report.len > READ_CHUNK_MAX as u32);
        let mut first = [0u8; 600];
        let mut second = [0u8; 600];
        let a = session.read(epoch, 0, 4_000, &mut first).unwrap();
        let b = session.read(epoch, 0, READ_CHUNK_MAX, &mut second).unwrap();
        assert_eq!(a.copied, READ_CHUNK_MAX);
        assert_eq!(a, b);
        assert_eq!(first[..READ_CHUNK_MAX], second[..READ_CHUNK_MAX]);
        assert_eq!(a.digest, report.digest);
        let mut tail = [0u8; 128];
        let rest = session
            .read(epoch, READ_CHUNK_MAX as u32, READ_CHUNK_MAX, &mut tail)
            .unwrap();
        assert_eq!(rest.copied as u32, report.len - READ_CHUNK_MAX as u32);
        assert_eq!(
            session.read(epoch, report.len + 1, 8, &mut tail),
            Err(PerfError::OffsetOutOfRange)
        );
    }

    #[test]
    fn an_old_epoch_cannot_end_a_span_in_the_new_run() {
        let mut session = session();
        let first = arm_trace(&mut session, 1);
        session
            .push_event(first, event(TraceKind::Begin, 8, 1))
            .unwrap();
        session.freeze(first, 50).unwrap();
        session.release(first).unwrap();

        let second = arm_trace(&mut session, 2);
        session
            .push_event(second, event(TraceKind::Begin, 8, 2))
            .unwrap();
        assert_eq!(
            session.push_event(first, event(TraceKind::End, 8, 3)),
            Err(PerfError::WrongEpoch)
        );
        let report = session.freeze(second, 80).unwrap();
        assert_eq!(report.flags & FLAG_INCOMPLETE, FLAG_INCOMPLETE);
        assert_eq!(report.len, (TRACE_HEADER_LEN + TRACE_EVENT_LEN) as u32);
    }

    #[test]
    fn snapshots_need_the_same_boot_epoch_and_profile() {
        let mut session = session();
        let epoch = arm_trace(&mut session, 7);
        let stamp = session.stamp().unwrap();
        assert_eq!(stamp.epoch, epoch);
        let other_profile = RunStamp {
            profile_hash: [0; PROFILE_HASH_LEN],
            ..stamp
        };
        assert!(!snapshots_comparable(&stamp, &other_profile));
        let other_boot = RunStamp {
            boot_id: stamp.boot_id + 1,
            ..stamp
        };
        assert!(!snapshots_comparable(&stamp, &other_boot));
        assert!(snapshots_comparable(&stamp, &stamp));
        session.freeze(epoch, 10).unwrap();
        session.release(epoch).unwrap();
        assert!(session.stamp().is_none());
    }
}
