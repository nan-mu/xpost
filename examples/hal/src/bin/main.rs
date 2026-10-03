#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use hal::connection::{connection, net_task};
use hal::init_log;
use hal::log_gate::init_logger;
use hal::sta;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("PANIC {panic_info}");
    esp_hal::system::software_reset();
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) {
    init_logger();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);
    init_log!("[Init][Embassy] Embassy initialized!");

    let (controller, sta_stack, sta_runner) = sta::init_sta(peripherals.WIFI);

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(sta_runner).unwrap());

    sta::start_udp_tasks(spawner, sta_stack);
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
