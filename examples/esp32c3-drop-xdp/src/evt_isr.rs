//! Software interrupt: drain `dut_evt` ringbuf → registered handlers.

use core::cell::RefCell;

use critical_section::Mutex;
use esp_hal::interrupt::software::SoftwareInterrupt;

static EVT_IRQ: Mutex<RefCell<Option<SoftwareInterrupt<'static, 1>>>> = Mutex::new(RefCell::new(None));

/// Install SW interrupt 1 handler (SW0 = Embassy scheduler).
pub fn init(mut irq: SoftwareInterrupt<'static, 1>) {
    irq.set_interrupt_handler(evt_isr);
    critical_section::with(|cs| {
        EVT_IRQ.borrow_ref_mut(cs).replace(irq);
    });
    crate::init_log!("[drop_xdp] dut_evt ring → SW IRQ drain");
}

/// Pend IRQ after XDP `bpf_xpost_evt_wake()` (bench_done).
pub fn pend() {
    critical_section::with(|cs| {
        if let Some(irq) = EVT_IRQ.borrow_ref_mut(cs).as_mut() {
            irq.raise();
        }
    });
}

#[esp_hal::handler]
fn evt_isr() {
    let n = xpost::evt::drain_isr();
    if n > 0 {
        crate::trace_log!("[drop_xdp] evt drain n={n}");
    }
    critical_section::with(|cs| {
        if let Some(irq) = EVT_IRQ.borrow_ref_mut(cs).as_mut() {
            irq.reset();
        }
    });
}
