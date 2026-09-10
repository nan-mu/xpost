//! `dut_evt` async consumer — [`RingbufRx`] + `embassy_futures::select`.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use xpost::evt;
use xpost::map::{async_ringbuf::traits::AsyncConsumer, ringbuf_record_payload, RingbufRx};

const TICK_MS: u64 = 100;

#[embassy_executor::task]
pub async fn run(mut rx: RingbufRx) {
    loop {
        match select(rx.pop(), Timer::after(Duration::from_millis(TICK_MS))).await {
            Either::First(Some(record)) => {
                if let Some(payload) = ringbuf_record_payload(&record) {
                    evt::dispatch_record(payload);
                }
            }
            Either::First(None) => break,
            Either::Second(_) => {
                let _ = evt::drain_isr();
            }
        }
    }
}
