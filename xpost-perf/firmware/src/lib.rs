//! Network measurement over the XPOST serial session commands.

#![no_std]

use core::cell::RefCell;

use critical_section::Mutex;
use embeprom::Counter;
use xpost_perf::network::{DICTIONARY_VERSION, probe_sequence, stimulus_payload};
use xpost_perf::session::{PerfError as SessionError, PerfSession, READ_CHUNK_MAX, Variant};
use xpost_perf::trace::{TraceEvent, TraceKind};
use xpost_serial::command::PerfVariant;
use xpost_serial::reply::{OkReply, PerfError};
use xpost_serial::server::ReplyWriter;
use xpost_type::RxTag;

pub use xpost_perf::network::Point;

pub const METRICS_BYTES: usize = 1536;
pub const TRACE_CAPACITY: usize = 64;
const TICK_HZ: u64 = 1_000_000;

pub type HalPerf = NetworkPerf<TRACE_CAPACITY, METRICS_BYTES>;

/// Holds cumulative counters and one retained export.
pub struct NetworkPerf<const N: usize, const M: usize> {
    points: &'static [Point],
    counters: [Counter; Point::COUNT],
    capture: Mutex<RefCell<Capture<N, M>>>,
}

struct Capture<const N: usize, const M: usize> {
    session: PerfSession<N, M>,
    sequence: u32,
}

impl<const N: usize, const M: usize> NetworkPerf<N, M> {
    pub const fn new(points: &'static [Point]) -> Self {
        if M != 0 {
            let mut bytes = xpost_perf::snapshot::SNAPSHOT_HEADER_LEN + xpost_perf::HEADER_LEN;
            let mut i = 0;
            while i < points.len() {
                bytes += 10 + points[i].name().len();
                i += 1;
            }
            assert!(M >= 2 * bytes);
        }
        Self {
            points,
            counters: [const { Counter::new() }; Point::COUNT],
            capture: Mutex::new(RefCell::new(Capture {
                session: PerfSession::new(0, TICK_HZ, 64, DICTIONARY_VERSION),
                sequence: 0,
            })),
        }
    }

    /// Set the boot ID before starting the network or serial tasks.
    pub fn init(&self) -> u32 {
        let boot_id = esp_hal::rng::Rng::new().random().max(1);
        critical_section::with(|cs| {
            self.capture
                .borrow_ref_mut(cs)
                .session
                .set_boot_id(u64::from(boot_id))
        })
        .expect("network perf initializes once per boot");
        boot_id
    }

    /// Identify a packet at driver receive before the runtime modifies it.
    pub fn driver_rx(&self, frame: &[u8], tag_offset: usize) -> Option<RxTag> {
        self.count(Point::DriverRx);
        self.begin_packet(1, Point::DriverRx, frame.len(), || {
            stimulus_payload(frame)?.get(tag_offset..)
        })
    }

    /// Record a driver event. Asynchronous hooks supply no tag.
    pub fn driver_event(&self, tag: Option<RxTag>, point: Point, arg0: u32) {
        self.count(point);
        if let Some(tag) = tag {
            self.record_packet(tag, point, 1, arg0);
        }
    }

    /// Identify a UDP datagram at socket receive.
    pub fn socket_rx(&self, payload: &[u8], tag_offset: usize) -> Option<RxTag> {
        self.count(Point::SocketRx);
        self.begin_packet(2, Point::SocketRx, payload.len(), || {
            payload.get(tag_offset..)
        })
    }

    /// Record a socket outcome at its completion boundary.
    pub fn socket_event(&self, point: Point, tag: Option<RxTag>) {
        self.count(point);
        if let Some(tag) = tag {
            self.record_packet(tag, point, 2, 0);
        }
    }

    fn begin_packet<'a>(
        &self,
        context: u8,
        point: Point,
        len: usize,
        payload: impl FnOnce() -> Option<&'a [u8]>,
    ) -> Option<RxTag> {
        critical_section::with(|cs| {
            let mut capture = self.capture.borrow_ref_mut(cs);
            let (epoch, uuid) = capture.session.active_run()?;
            let sequence = probe_sequence(payload()?, uuid)?;
            let tag = RxTag { epoch, sequence };
            self.observe_packet(&mut capture, tag, point, context, len as u32);
            Some(tag)
        })
    }

    fn record_packet(&self, tag: RxTag, point: Point, context: u8, arg0: u32) {
        critical_section::with(|cs| {
            let mut capture = self.capture.borrow_ref_mut(cs);
            if capture
                .session
                .active_run()
                .is_some_and(|(epoch, _)| epoch == tag.epoch)
            {
                self.observe_packet(&mut capture, tag, point, context, arg0);
            }
        });
    }

    fn observe_packet(
        &self,
        capture: &mut Capture<N, M>,
        tag: RxTag,
        point: Point,
        context: u8,
        arg0: u32,
    ) {
        if capture.session.variant() == Variant::Trace {
            capture.record(tag.epoch, point, context, tag.sequence, arg0);
        } else {
            let counter = match point {
                Point::DriverRx => Point::TaggedDriverRx,
                Point::DriverNormalDrop => Point::TaggedDriverNormalDrop,
                Point::DriverPacketFault => Point::TaggedDriverFault,
                Point::XdpPolicyReturned => Point::TaggedPolicyReturned,
                Point::SocketRx => Point::TaggedSocketRx,
                Point::SocketDiscarded => Point::TaggedSocketDiscarded,
                Point::SocketAddressRejected => Point::TaggedSocketFault,
                _ => return,
            };
            self.count(counter);
        }
    }

    #[inline]
    fn count(&self, point: Point) {
        if M != 0
            && !matches!(
                point,
                Point::DriverRxEnd
                    | Point::DriverTxSubmitBegin
                    | Point::SocketSendBegin
                    | Point::DriverNormalDrop
                    | Point::DriverPacketFault
                    | Point::DriverTxEnqueued
                    | Point::XdpPolicyReturned
            )
        {
            self.counters[point as usize - 1].inc();
        }
    }

    fn counters(&self) -> impl ExactSizeIterator<Item = (&'static str, u64)> {
        self.points
            .iter()
            .map(|&point| (point.name(), self.counters[point as usize - 1].get()))
    }

    pub fn prepare(
        &self,
        uuid: [u8; 16],
        variant: PerfVariant,
        profile: [u8; 32],
        reply: ReplyWriter<'_>,
    ) {
        let variant = match variant {
            PerfVariant::Metrics => Variant::Metrics,
            PerfVariant::Trace => Variant::Trace,
        };
        let result = critical_section::with(|cs| {
            let mut capture = self.capture.borrow_ref_mut(cs);
            let prepared = capture.session.prepare(uuid, variant, profile)?;
            capture.sequence = 0;
            Ok(prepared)
        });
        match result {
            Ok((epoch, boot_id)) => reply.ok(OkReply::PerfPrepare { epoch, boot_id }),
            Err(error) => reply.error(&wire_error(error)),
        }
    }

    pub fn arm(&self, epoch: u32, reply: ReplyWriter<'_>) {
        let result = critical_section::with(|cs| {
            let mut capture = self.capture.borrow_ref_mut(cs);
            let tick = now();
            match capture.session.variant() {
                Variant::Metrics => capture.session.arm_metrics(epoch, tick, self.counters()),
                Variant::Trace => capture.session.arm(epoch, tick),
            }
        });
        match result {
            Ok(tick) => reply.ok(OkReply::PerfArm { tick }),
            Err(error) => reply.error(&wire_error(error)),
        }
    }

    pub fn freeze(&self, epoch: u32, reply: ReplyWriter<'_>) {
        let result = critical_section::with(|cs| {
            let mut capture = self.capture.borrow_ref_mut(cs);
            let tick = now();
            match capture.session.variant() {
                Variant::Metrics => capture.session.freeze_metrics(epoch, tick, self.counters()),
                Variant::Trace => capture.session.freeze(epoch, tick),
            }
        });
        match result {
            Ok(value) => reply.ok(OkReply::PerfFreeze {
                tick: value.freeze_tick,
                flags: value.flags,
                len: value.len,
                digest: value.digest,
            }),
            Err(error) => reply.error(&wire_error(error)),
        }
    }

    pub fn read(&self, epoch: u32, offset: u32, max_bytes: u32, reply: ReplyWriter<'_>) {
        let mut bytes = [0; READ_CHUNK_MAX];
        let result = critical_section::with(|cs| {
            self.capture
                .borrow_ref(cs)
                .session
                .read(epoch, offset, max_bytes as usize, &mut bytes)
        });
        match result {
            Ok(chunk) => reply.ok(OkReply::PerfRead {
                total: chunk.total,
                digest: chunk.digest,
                blob: &bytes[..chunk.copied],
            }),
            Err(error) => reply.error(&wire_error(error)),
        }
    }

    pub fn release(&self, epoch: u32, reply: ReplyWriter<'_>) {
        let result =
            critical_section::with(|cs| self.capture.borrow_ref_mut(cs).session.release(epoch));
        match result {
            Ok(()) => reply.ok(OkReply::PerfRelease),
            Err(error) => reply.error(&wire_error(error)),
        }
    }
}

impl<const N: usize, const M: usize> Capture<N, M> {
    fn record(&mut self, epoch: u32, point: Point, context_id: u8, probe_seq: u64, arg0: u32) {
        let event = TraceEvent {
            sequence: self.sequence,
            event_id: point as u16,
            kind: TraceKind::Instant,
            context_id,
            tick: now(),
            probe_seq,
            span_id: 0,
            parent_span_id: 0,
            arg0,
            arg1: 0,
        };
        if self.session.push_event(epoch, event).is_ok() {
            self.sequence = self.sequence.saturating_add(1);
        }
    }
}

fn now() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

fn wire_error(error: SessionError) -> PerfError {
    match error {
        SessionError::WrongEpoch => PerfError::Epoch,
        SessionError::OffsetOutOfRange => PerfError::Offset,
        SessionError::Unsupported => PerfError::Unsupported,
        SessionError::InvalidSnapshot => PerfError::BufferTooSmall,
        _ => PerfError::Phase,
    }
}

/// Attach the five session commands to an existing serial builder.
#[macro_export]
macro_rules! register {
    ($builder:expr, $perf:ident) => {
        $builder
            .on_perf_prepare(|uuid, variant, profile, reply| {
                $perf.prepare(uuid, variant, profile, reply)
            })
            .on_perf_arm(|epoch, reply| $perf.arm(epoch, reply))
            .on_perf_freeze(|epoch, reply| $perf.freeze(epoch, reply))
            .on_perf_read(|epoch, offset, max_bytes, reply| {
                $perf.read(epoch, offset, max_bytes, reply)
            })
            .on_perf_release(|epoch, reply| $perf.release(epoch, reply))
    };
}
