//! Firmware event ring (feature `event-ringbuf`).
//!
//! Consumes `BPF_MAP_TYPE_RINGBUF` records for DUT events. BPF `bpf_ringbuf_*`
//! helpers stay deferred.

use core::cell::RefCell;

use critical_section::Mutex;

use crate::map::kinds::ringbuf::RingbufMap;
use crate::map::runtime::pool::{MapKind, ResourceId, pool_try_write};

/// `struct xpost_evt_feed` in `xdp_drop.c`
pub const EVT_FEED: u32 = 1;
/// `struct xpost_evt_bench` — cap reached; host should stop blast.
pub const EVT_BENCH_DONE: u32 = 2;
/// Idle notification (board-defined).
pub const EVT_IDLE: u32 = 3;

pub const FEED_REC_LEN: usize = 8;
pub const BENCH_REC_LEN: usize = 32;

type WakeHook = fn();
type BenchHook = fn(u64, u64, u64);
type IdleHook = fn();
type FeedHook = fn(u64);

struct State {
    ring_id: Option<ResourceId>,
    wake: Option<WakeHook>,
    on_bench_done: Option<BenchHook>,
    on_idle: Option<IdleHook>,
    on_feed: Option<FeedHook>,
}

static STATE: Mutex<RefCell<State>> = Mutex::new(RefCell::new(State {
    ring_id: None,
    wake: None,
    on_bench_done: None,
    on_idle: None,
    on_feed: None,
}));

/// Bind the DUT event ring by registered map name.
pub fn bind_ring_named(name: &str) {
    if let Some(id) = crate::map::resolve_map_name(name) {
        bind_ring(id);
    }
}

/// Bind the DUT event ring (`ResourceId` from [`crate::map::resolve_map_name`]).
pub fn bind_ring(id: ResourceId) {
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).ring_id = Some(id);
    });
}

/// Board hook: pend software interrupt when a record needs immediate service.
pub fn set_wake_hook(hook: WakeHook) {
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).wake = Some(hook);
    });
}

pub fn set_bench_done_hook(hook: BenchHook) {
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).on_bench_done = Some(hook);
    });
}

pub fn set_idle_hook(hook: IdleHook) {
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).on_idle = Some(hook);
    });
}

pub fn set_feed_hook(hook: FeedHook) {
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).on_feed = Some(hook);
    });
}

/// Called from [`crate::map::kinds::ringbuf`] after a record is committed.
pub fn on_ring_produced() {}

/// Unsupported BPF helper leftover: `bpf_xpost_evt_wake` is not dispatched.
#[allow(dead_code)]
pub fn helper_wake() -> usize {
    usize::MAX
}

pub fn emit_idle() {
    let mut rec = [0u8; FEED_REC_LEN];
    rec[0..4].copy_from_slice(&EVT_IDLE.to_le_bytes());
    let _ = output_record(&rec);
    fire_wake();
}

/// Drain pending records (ISR / timer). Returns number dispatched.
pub fn drain_isr() -> usize {
    let ring_id = critical_section::with(|cs| STATE.borrow_ref(cs).ring_id);
    let Some(ring_id) = ring_id else {
        return 0;
    };
    let mut buf = [0u8; 64];
    let mut n = 0usize;
    loop {
        let Some(len) = try_pop_record(ring_id, &mut buf) else {
            break;
        };
        dispatch(&buf[..len]);
        n += 1;
    }
    n
}

fn output_record(rec: &[u8]) -> Result<(), ()> {
    let ring_id = critical_section::with(|cs| STATE.borrow_ref(cs).ring_id).ok_or(())?;
    let mut guard = pool_try_write().ok_or(())?;
    let mut table = guard.as_mut().ok_or(())?;
    let mut res = table.get_mut(&ring_id).ok_or(())?;
    if res.kind() != Some(MapKind::Ringbuf) {
        return Err(());
    }
    let rb: &mut RingbufMap = unsafe { res.as_mut() };
    if rb.bpf_mut().output(rec.as_ptr(), rec.len()) != 0 {
        return Err(());
    }
    Ok(())
}

fn try_pop_record(ring_id: ResourceId, out: &mut [u8]) -> Option<usize> {
    let mut guard = pool_try_write()?;
    let mut table = guard.as_mut()?;
    let mut res = table.get_mut(&ring_id)?;
    if res.kind() != Some(MapKind::Ringbuf) {
        return None;
    }
    let rb: &mut RingbufMap = unsafe { res.as_mut() };
    rb.bpf_mut().try_consume(out)
}

/// Decode one ringbuf record (feed / bench_done / idle hooks).
pub fn dispatch_record(buf: &[u8]) {
    dispatch(buf);
}

fn dispatch(buf: &[u8]) {
    if buf.len() < 4 {
        return;
    }
    let kind = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    match kind {
        EVT_FEED => {
            if buf.len() >= FEED_REC_LEN {
                let us = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
                let hook = critical_section::with(|cs| STATE.borrow_ref(cs).on_feed);
                if let Some(h) = hook {
                    h(us);
                }
            }
        }
        EVT_BENCH_DONE => {
            if buf.len() >= BENCH_REC_LEN {
                let count = u64::from_le_bytes(buf[8..16].try_into().unwrap());
                let t0 = u64::from_le_bytes(buf[16..24].try_into().unwrap());
                let t1 = u64::from_le_bytes(buf[24..32].try_into().unwrap());
                let hook = critical_section::with(|cs| STATE.borrow_ref(cs).on_bench_done);
                if let Some(h) = hook {
                    h(count, t0, t1);
                }
            }
            fire_wake();
        }
        EVT_IDLE => {
            let hook = critical_section::with(|cs| STATE.borrow_ref(cs).on_idle);
            if let Some(h) = hook {
                h();
            }
        }
        _ => {}
    }
}

fn fire_wake() {
    let hook = critical_section::with(|cs| STATE.borrow_ref(cs).wake);
    if let Some(h) = hook {
        h();
    }
}
