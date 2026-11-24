//! Export switch events from one armed Trace epoch.

use core::cell::RefCell;

use critical_section::Mutex;
use xpost::xdp::SwitchReport;
use xpost_perf::session::{PerfError as SessionError, PerfSession, READ_CHUNK_MAX, Variant};
use xpost_perf::trace::{PROBE_UNATTRIBUTED, TraceEvent, TraceKind};
use xpost_serial::command::PerfVariant;
use xpost_serial::reply::{OkReply, PerfError};
use xpost_serial::server::ReplyWriter;

const TICK_HZ: u64 = 1_000_000;
const DICTIONARY: u16 = 1;
static SESSION: Mutex<RefCell<PerfSession<16>>> =
    Mutex::new(RefCell::new(PerfSession::new(0, TICK_HZ, 64, DICTIONARY)));

pub fn init(boot_id: u32) {
    critical_section::with(|cs| {
        SESSION.replace(
            cs,
            PerfSession::new(u64::from(boot_id), TICK_HZ, 64, DICTIONARY),
        );
    });
}

pub fn now() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

pub fn prepare(uuid: [u8; 16], variant: PerfVariant, profile: [u8; 32], reply: ReplyWriter<'_>) {
    if variant != PerfVariant::Trace {
        return reply.error(&PerfError::Unsupported);
    }
    let result = critical_section::with(|cs| {
        SESSION
            .borrow_ref_mut(cs)
            .prepare(uuid, Variant::Trace, profile)
    });
    match result {
        Ok((epoch, boot_id)) => reply.ok(OkReply::PerfPrepare { epoch, boot_id }),
        Err(error) => reply.error(&wire_error(error)),
    }
}

pub fn arm(epoch: u32, reply: ReplyWriter<'_>) {
    let result = critical_section::with(|cs| SESSION.borrow_ref_mut(cs).arm(epoch, now()));
    match result {
        Ok(tick) => reply.ok(OkReply::PerfArm { tick }),
        Err(error) => reply.error(&wire_error(error)),
    }
}

pub fn freeze(epoch: u32, reply: ReplyWriter<'_>) {
    let result = critical_section::with(|cs| SESSION.borrow_ref_mut(cs).freeze(epoch, now()));
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

pub fn read(epoch: u32, offset: u32, max_bytes: u32, reply: ReplyWriter<'_>) {
    let mut bytes = [0; READ_CHUNK_MAX];
    let result = critical_section::with(|cs| {
        SESSION
            .borrow_ref(cs)
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

pub fn release(epoch: u32, reply: ReplyWriter<'_>) {
    let result = critical_section::with(|cs| SESSION.borrow_ref_mut(cs).release(epoch));
    match result {
        Ok(()) => reply.ok(OkReply::PerfRelease),
        Err(error) => reply.error(&wire_error(error)),
    }
}

pub fn switch(begin: u64, slot: u8, generation: u32, report: Option<&SwitchReport>) {
    let end = now();
    critical_section::with(|cs| {
        let mut session = SESSION.borrow_ref_mut(cs);
        let epoch = session.epoch();
        let mut event = |id, kind, tick, sequence| {
            let _ = session.push_event(
                epoch,
                TraceEvent {
                    sequence,
                    event_id: id,
                    kind,
                    context_id: 0,
                    tick,
                    probe_seq: PROBE_UNATTRIBUTED,
                    span_id: u32::from(id),
                    parent_span_id: if id == 1 { 0 } else { 1 },
                    arg0: u32::from(slot),
                    arg1: generation,
                },
            );
        };
        event(1, TraceKind::Begin, begin, 0);
        if let Some(timing) = report.and_then(|report| report.timing) {
            event(2, TraceKind::Begin, timing.quiesce_begin, 1);
            event(2, TraceKind::End, timing.quiesce_end, 2);
            event(3, TraceKind::Begin, timing.rebind_begin, 3);
            event(3, TraceKind::End, timing.rebind_end, 4);
            event(4, TraceKind::Begin, timing.publish_begin, 5);
            event(4, TraceKind::End, timing.before_admission, 6);
        }
        event(1, TraceKind::End, end, 7);
    });
}

fn wire_error(error: SessionError) -> PerfError {
    match error {
        SessionError::WrongEpoch => PerfError::Epoch,
        SessionError::OffsetOutOfRange => PerfError::Offset,
        _ => PerfError::Phase,
    }
}
