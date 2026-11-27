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

// Agent verifies this identity against the selected profile before a write.
#[unsafe(no_mangle)]
pub static XPOST_EXB2_HAL_TASK: u8 = exec_bench_hal::TASK_ID;

static mut EXEC_BATCH: ExecBatch = ExecBatch::new();

#[allow(
    clippy::large_stack_frames,
    reason = "embassy main allocates larger boot buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    core::hint::black_box(&XPOST_EXB2_HAL_TASK);
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
        .on_exec_batch(move |command, reply| batch.handle(command, reply, boot_id, |buf| Ok(run_exb2(buf))));
        exec_metrics::register(builder).build()
    }
}

fn run_exb2(buffer: &mut [u8]) -> (u64, u64, u32) {
    exec_metrics::begin_invocation();
    let begin = exec_metrics::now();
    let status = exec_bench_hal::execute(buffer);
    let end = exec_metrics::now();
    let action = if status == 0 {
        xpost_type::XdpAction::Pass
    } else {
        xpost_type::XdpAction::Aborted
    };
    exec_metrics::end_invocation(action);
    (begin, end, status)
}
