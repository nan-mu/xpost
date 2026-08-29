//! TIMG1 periodic ISR — delegates to [`xpost::watchdog::tick_isr`].

use core::cell::RefCell;

use critical_section::Mutex;
use esp_hal::time::Duration;
use esp_hal::timer::PeriodicTimer;
use esp_hal::timer::timg::TimerGroup;

/// Firmware poll period; keep in sync with `test_common::drop_stats::IDLE_WATCHDOG_POLL_MS`.
const POLL_MS: u64 = 100;

static POLL_TIMER: Mutex<RefCell<Option<PeriodicTimer<'static, esp_hal::Blocking>>>> =
    Mutex::new(RefCell::new(None));

pub fn init(timg1: esp_hal::peripherals::TIMG1<'static>) {
    let group = TimerGroup::new(timg1);
    let mut timer = PeriodicTimer::new(group.timer0);
    timer.set_interrupt_handler(poll_isr);
    timer.listen();
    timer
        .start(Duration::from_millis(POLL_MS))
        .expect("idle_wdt start");
    critical_section::with(|cs| {
        POLL_TIMER.borrow_ref_mut(cs).replace(timer);
    });
    crate::init_log!("[drop_xdp] hw tick → dut_evt drain + watchdog {}ms", POLL_MS);
}

#[esp_hal::handler]
fn poll_isr() {
    let _ = xpost::evt::drain_isr();
    xpost::watchdog::tick_isr();
    critical_section::with(|cs| {
        if let Some(timer) = POLL_TIMER.borrow_ref_mut(cs).as_mut() {
            timer.clear_interrupt();
        }
    });
}
