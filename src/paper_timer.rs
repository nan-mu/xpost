//! Paper Listing 1 blast window: arm a timer, then auto-disable datapath.
//!
//! Host loads XDP via normal HotLoad/MapCrud while datapath is off, then
//! `KIND_DATAPATH` + `DATAPATH_ENABLE_FOR` with `dram_slot = duration_ms`.
//! This task sleeps that many ms and calls `datapath::disable()` so control
//! UDP reaches userspace again without punching a hole in the XDP program.

use core::cell::Cell;

use critical_section::Mutex;
use embassy_time::{Duration, Timer};
use log::info;
use xdp_mcu::datapath;

/// `(deadline_ms, ticket)` — ticket bumps on each arm to cancel in-flight waits.
static ARM: Mutex<Cell<(u32, u32)>> = Mutex::new(Cell::new((0, 0)));

/// Schedule datapath disable after `ms` milliseconds (ticket-cancelled).
pub fn arm_disable_after_ms(ms: u32) {
    if ms == 0 {
        return;
    }
    critical_section::with(|cs| {
        let cell = ARM.borrow(cs);
        let (_, ticket) = cell.get();
        cell.set((ms, ticket.wrapping_add(1)));
    });
}

#[embassy_executor::task]
pub async fn paper_datapath_timer_task() {
    info!("[PAPER] datapath timer task up");
    loop {
        // Claim one arm: take ms and clear the slot under CS.
        let (ms, ticket) = loop {
            let claimed = critical_section::with(|cs| {
                let cell = ARM.borrow(cs);
                let (ms, ticket) = cell.get();
                if ms == 0 {
                    None
                } else {
                    cell.set((0, ticket));
                    Some((ms, ticket))
                }
            });
            if let Some(pair) = claimed {
                break pair;
            }
            Timer::after(Duration::from_millis(20)).await;
        };

        Timer::after(Duration::from_millis(ms as u64)).await;
        let (_, now_ticket) = critical_section::with(|cs| ARM.borrow(cs).get());
        if now_ticket == ticket {
            datapath::disable();
            info!("[PAPER] auto-disable after {ms} ms");
        }
    }
}
