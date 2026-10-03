//! Async RX side for BPF ringbufs — [`RingbufReceiver`] works with `embassy` `select!`.
//!
//! XDP `ringbuf_output` / `submit` calls [`notify_produced`] to wake a registered waker.
//! The consumer task polls via [`RingbufReceiver::recv`] without blocking the datapath.

use core::cell::RefCell;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use critical_section::Mutex;
use embassy_sync::waitqueue::AtomicWaker;

use super::pool::{MapKind, POOL, ResourceId};
use super::ringbuf::BpfRingBuf;

/// Max BPF ringbuf record bytes (matches [`super::ringbuf`]).
pub const MAX_RECORD_BYTES: usize = 256;

/// One length-prefixed record from a map ringbuf.
pub struct RingbufRecord {
    pub bytes: heapless::Vec<u8, MAX_RECORD_BYTES>,
}

/// Async reader for a registered `BPF_MAP_TYPE_RINGBUF`.
///
/// Created via [`super::builder::RingbufMap::into_channel`]. Safe to use in
/// `embassy_futures::select::select` alongside other futures.
pub struct RingbufReceiver {
    id: ResourceId,
}

struct RxSlot {
    waker: AtomicWaker,
}

static RX: Mutex<RefCell<[Option<RxSlot>; super::ctrl::MAX_MAP_IDS as usize]>> =
    Mutex::new(RefCell::new([const { None }; super::ctrl::MAX_MAP_IDS as usize]));

/// Attach an async consumer to a ringbuf [`ResourceId`].
pub(crate) fn attach_rx(id: ResourceId) -> RingbufReceiver {
    critical_section::with(|cs| {
        let mut table = RX.borrow_ref_mut(cs);
        let idx = id as usize;
        if idx < table.len() {
            table[idx] = Some(RxSlot {
                waker: AtomicWaker::new(),
            });
        }
    });
    RingbufReceiver { id }
}

/// Wake the async consumer after XDP / helper committed a record.
pub fn notify_produced(id: ResourceId) {
    critical_section::with(|cs| {
        let table = RX.borrow_ref(cs);
        if let Some(Some(slot)) = table.get(id as usize) {
            slot.waker.wake();
        }
    });
}

impl RingbufReceiver {
    /// Map pool id (same as [`super::resolve_map_name`] after register).
    pub fn id(&self) -> ResourceId {
        self.id
    }

    /// Non-blocking pop; returns `None` if empty or pool busy.
    pub fn try_recv(&mut self) -> Option<RingbufRecord> {
        let mut scratch = [0u8; MAX_RECORD_BYTES];
        let len = try_pop_record(self.id, &mut scratch)?;
        let mut bytes = heapless::Vec::new();
        let _ = bytes.extend_from_slice(&scratch[..len]);
        Some(RingbufRecord { bytes })
    }

    /// Wait for the next record (`Poll::Pending` until XDP produces or waker fired).
    pub fn recv(&mut self) -> Recv<'_> {
        Recv { rx: self }
    }
}

/// Future returned by [`RingbufReceiver::recv`].
pub struct Recv<'a> {
    rx: &'a mut RingbufReceiver,
}

impl Future for Recv<'_> {
    type Output = Option<RingbufRecord>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let rx = &mut self.get_mut().rx;
        if let Some(rec) = rx.try_recv() {
            return Poll::Ready(Some(rec));
        }
        let idx = rx.id as usize;
        critical_section::with(|cs| {
            let table = RX.borrow_ref(cs);
            if let Some(Some(slot)) = table.get(idx) {
                slot.waker.register(cx.waker());
            }
        });
        Poll::Pending
    }
}

fn try_pop_record(ring_id: ResourceId, out: &mut [u8]) -> Option<usize> {
    let mut guard = POOL.try_write()?;
    let table = guard.as_mut()?;
    let res = table.get_mut(&ring_id)?;
    if res.kind != Some(MapKind::Ringbuf) {
        return None;
    }
    let rb: &mut BpfRingBuf = unsafe { res.as_mut() };
    rb.try_consume(out)
}
