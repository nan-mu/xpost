//! `dut_evt` async consumer — [`RingbufReceiver`] + `embassy_futures::select`.

use embassy_futures::select::{Either, select};
use embassy_time::{Duration, Timer};
use xpost::evt;
use xpost::map::RingbufReceiver;
use xpost::watchdog;

const TICK_MS: u64 = 100;

#[embassy_executor::task]
pub async fn run(mut rx: RingbufReceiver) {
    loop {
        match select(rx.recv(), Timer::after(Duration::from_millis(TICK_MS))).await {
            Either::First(Some(rec)) => evt::dispatch_record(&rec.bytes),
            Either::First(None) => break,
            Either::Second(_) => watchdog::tick_isr(),
        }
    }
}
