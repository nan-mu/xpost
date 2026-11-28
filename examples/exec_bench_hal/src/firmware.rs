#[path = "../../exec_bench/src/exec_metrics.rs"]
mod exec_metrics;

use core::convert::Infallible;

use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::error;
use xpost_serial::server::SerialHandler;
use xpost_serial::server::exec_batch::{EXEC_BATCH_LINE_BYTES, ExecBatch};

use crate::{Workload, execute};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

esp_bootloader_esp_idf::esp_app_desc!();

pub async fn run<W: Workload>(task_marker: &'static u8) -> ! {
    core::hint::black_box(task_marker);
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
    let mut rx = usb_rx;
    let mut batch = ExecBatch::new();
    let builder = SerialHandler::builder(
        move || rx.read_byte().ok(),
        |bytes| {
            esp_println::Printer::write_bytes(bytes);
            Ok::<(), Infallible>(())
        },
        [0u8; EXEC_BATCH_LINE_BYTES],
    )
    .on_exec_batch(move |command, reply| {
        batch.handle(command, reply, boot_id, |buf| Ok(run_exb2::<W>(buf)));
    });
    exec_metrics::register(builder).build().run().await;

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

fn run_exb2<W: Workload>(buffer: &mut [u8]) -> (u64, u64, u32) {
    exec_metrics::begin_invocation();
    let begin = exec_metrics::now();
    let status = execute::<W>(buffer);
    let end = exec_metrics::now();
    let action = if status == 0 {
        xpost_type::XdpAction::Pass
    } else {
        xpost_type::XdpAction::Aborted
    };
    exec_metrics::end_invocation(action);
    (begin, end, status)
}
