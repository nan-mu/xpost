//! Forward synchronous receive observations to the firmware session.

use embassy_sync::once_lock::OnceLock;
use xpost_type::{RxEvent, RxTag, XdpAction};

pub use xpost_perf::network::{Point, XDP_POINTS};

type Start = fn(&[u8]) -> Option<RxTag>;
type Record = fn(Option<RxTag>, Point, u32);

struct Observer {
    start: Start,
    record: Record,
}

static OBSERVER: OnceLock<Observer> = OnceLock::new();

/// Install the observer before Wi-Fi starts.
pub fn install(start: Start, record: Record) {
    assert!(OBSERVER.init(Observer { start, record }).is_ok());
}

pub(super) fn start(frame: &[u8]) -> Option<RxTag> {
    (OBSERVER.try_get()?.start)(frame)
}

pub(super) fn rx_event(tag: Option<RxTag>, event: RxEvent) {
    let (point, detail) = match event {
        RxEvent::RunEnter => (Point::XdpRunEnter, 0),
        RxEvent::Action(action) => {
            let point = match action {
                XdpAction::Pass => Point::XdpPass,
                XdpAction::Drop => Point::XdpDrop,
                XdpAction::Tx => Point::XdpTx,
                XdpAction::Aborted => Point::XdpAborted,
            };
            (point, 0)
        }
        RxEvent::PolicyReturned => (Point::XdpPolicyReturned, 0),
        RxEvent::Fault => (Point::DriverPacketFault, 0),
        RxEvent::StackQueued(depth) => (Point::DriverRxEnqueued, depth as u32),
        RxEvent::StackFull(depth) => (Point::DriverRxOverflow, depth as u32),
        RxEvent::Discarded { normal } => {
            record(tag, Point::DriverRxDiscarded, 0);
            if !normal {
                return;
            }
            (Point::DriverNormalDrop, 0)
        }
        RxEvent::TxQueued => (Point::DriverTxEnqueued, 0),
        RxEvent::TxQueueFailed => (Point::DriverTxEnqueueFailed, 0),
        RxEvent::End => (Point::DriverRxEnd, 0),
    };
    record(tag, point, detail);
}

#[inline]
pub(super) fn observe(point: Point, detail: u32) {
    record(None, point, detail);
}

#[inline]
fn record(tag: Option<RxTag>, point: Point, detail: u32) {
    if let Some(observer) = OBSERVER.try_get() {
        (observer.record)(tag, point, detail);
    }
}
