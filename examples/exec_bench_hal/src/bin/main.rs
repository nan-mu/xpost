#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
)]
#![deny(clippy::large_stack_frames)]

//! Native HAL twin of `exec_bench`. Run the selected EXB2 kernel directly.

#[cfg(feature = "diag")]
mod diagnostics;
mod exb2;
#[path = "../../../exec_bench/src/exec_metrics.rs"]
mod exec_metrics;

use core::convert::Infallible;
use core::ptr::addr_of_mut;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::error;
use xpost_serial::server::exec_batch::{EXEC_BATCH_LINE_BYTES, ExecBatch};
use xpost_serial::server::{SerialHandler, serial_task};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

static mut EXEC_BATCH: ExecBatch = ExecBatch::new();

#[allow(
    clippy::large_stack_frames,
    reason = "embassy main allocates larger boot buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Info);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let boot_id = esp_hal::rng::Rng::new().random().max(1);
    exec_metrics::init(boot_id);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    // Host re-marks GetInit and reopens UART after espflash reset (~3s capture).
    // Emit `OK init` inside that window; immediate spawn races and is lost.
    Timer::after(Duration::from_millis(800)).await;
    // The single serial task owns the staging buffer for the rest of this boot.
    let batch = unsafe { &mut *addr_of_mut!(EXEC_BATCH) };
    spawner.spawn(control_serial(usb_rx, batch, boot_id).unwrap());

    // Scale sweep, for the three-engine comparison. Runs after the serial task
    // starts so a reset still produces the boot banner first.
    #[cfg(feature = "diag")]
    diagnostics::scale_sweep();

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

serial_task! {
    fn control_serial(
        rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
        batch: &'static mut ExecBatch,
        boot_id: u32,
    ) {
        let mut rx = rx;
        let builder = SerialHandler::builder(
            move || match rx.read_byte() {
                Ok(b) => Some(b),
                Err(_) => None,
            },
            |bytes| {
                esp_println::Printer::write_bytes(bytes);
                Ok::<(), Infallible>(())
            },
            [0u8; EXEC_BATCH_LINE_BYTES],
        )
        .on_exec_batch(move |command, reply| batch.handle(command, reply, boot_id, |buf| Ok(exb2::run(buf))));
        exec_metrics::register(builder).build()
    }
}
