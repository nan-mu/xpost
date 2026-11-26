//! Cumulative EXB2 counters and one retained Metrics export.

use core::cell::RefCell;

use critical_section::Mutex;
use embassy_time::Instant;
use embeprom::Counter;
use xpost_perf::session::{PerfError as SessionError, PerfSession, READ_CHUNK_MAX, Variant};
use xpost_perf::{HEADER_LEN, Kind, MAX_NAME, snapshot::SNAPSHOT_HEADER_LEN};
use xpost_serial::command::PerfVariant;
use xpost_serial::reply::{OkReply, PerfError};
use xpost_serial::server::serial::RequestHandler;
use xpost_serial::server::{ReplyWriter, SerialHandlerBuilder};
use xpost_type::XdpAction;

// Action entries follow the XdpAction discriminants.
const NAMES: [&str; 5] = [
    "exec_batch_invocations",
    "exec_batch_action_aborted",
    "exec_batch_action_drop",
    "exec_batch_action_pass",
    "exec_batch_action_tx",
];
const METRICS_BYTES: usize = 2
    * (SNAPSHOT_HEADER_LEN + HEADER_LEN + NAMES.len() * (2 + MAX_NAME + Kind::Counter.fixed_len()));

type Session = PerfSession<0, METRICS_BYTES>;
static SESSION: Mutex<RefCell<Session>> =
    Mutex::new(RefCell::new(PerfSession::new(0, 1_000_000, 64, 0)));
static COUNTERS: [Counter; NAMES.len()] = [const { Counter::new() }; NAMES.len()];

pub fn init(boot_id: u32) {
    with_session(|session| session.set_boot_id(u64::from(boot_id)))
        .expect("EXB2 Metrics initializes once per boot");
}

pub fn now() -> u64 {
    Instant::now().as_micros()
}

/// Count at the execution boundary, outside the timed interval.
pub fn begin_invocation() {
    COUNTERS[0].inc();
}

pub fn end_invocation(action: XdpAction) {
    COUNTERS[1 + action as usize].inc();
}

pub fn register<R, W, H: RequestHandler, const N: usize>(
    builder: SerialHandlerBuilder<R, W, H, N>,
) -> SerialHandlerBuilder<R, W, impl RequestHandler, N> {
    builder
        .on_perf_prepare(prepare)
        .on_perf_arm(arm)
        .on_perf_freeze(freeze)
        .on_perf_read(read)
        .on_perf_release(release)
}

fn prepare(uuid: [u8; 16], variant: PerfVariant, profile: [u8; 32], reply: ReplyWriter<'_>) {
    if variant == PerfVariant::Trace {
        return reply.error(&PerfError::Unsupported);
    }
    match with_session(|session| session.prepare(uuid, Variant::Metrics, profile)) {
        Ok((epoch, boot_id)) => reply.ok(OkReply::PerfPrepare { epoch, boot_id }),
        Err(error) => reply.error(&wire_error(error)),
    }
}

fn arm(epoch: u32, reply: ReplyWriter<'_>) {
    match with_session(|session| session.arm_metrics(epoch, now(), counters())) {
        Ok(tick) => reply.ok(OkReply::PerfArm { tick }),
        Err(error) => reply.error(&wire_error(error)),
    }
}

fn freeze(epoch: u32, reply: ReplyWriter<'_>) {
    match with_session(|session| session.freeze_metrics(epoch, now(), counters())) {
        Ok(report) => reply.ok(OkReply::PerfFreeze {
            tick: report.freeze_tick,
            flags: report.flags,
            len: report.len,
            digest: report.digest,
        }),
        Err(error) => reply.error(&wire_error(error)),
    }
}

fn read(epoch: u32, offset: u32, max_bytes: u32, reply: ReplyWriter<'_>) {
    let mut bytes = [0; READ_CHUNK_MAX];
    match with_session(|session| session.read(epoch, offset, max_bytes as usize, &mut bytes)) {
        Ok(chunk) => reply.ok(OkReply::PerfRead {
            total: chunk.total,
            digest: chunk.digest,
            blob: &bytes[..chunk.copied],
        }),
        Err(error) => reply.error(&wire_error(error)),
    }
}

fn release(epoch: u32, reply: ReplyWriter<'_>) {
    match with_session(|session| session.release(epoch)) {
        Ok(()) => reply.ok(OkReply::PerfRelease),
        Err(error) => reply.error(&wire_error(error)),
    }
}

fn with_session<T>(operation: impl FnOnce(&mut Session) -> T) -> T {
    critical_section::with(|cs| operation(&mut SESSION.borrow_ref_mut(cs)))
}

fn counters() -> impl ExactSizeIterator<Item = (&'static str, u64)> {
    NAMES.into_iter().zip(COUNTERS.iter().map(Counter::get))
}

fn wire_error(error: SessionError) -> PerfError {
    match error {
        SessionError::WrongEpoch => PerfError::Epoch,
        SessionError::OffsetOutOfRange => PerfError::Offset,
        SessionError::Unsupported => PerfError::Unsupported,
        SessionError::InvalidSnapshot => PerfError::BufferTooSmall,
        SessionError::Busy | SessionError::WrongPhase | SessionError::EpochExhausted => {
            PerfError::Phase
        }
    }
}
