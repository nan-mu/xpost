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
use log::{error, info};

use firmware::patch;

#[cfg(not(feature = "hw-selftest"))]
use firmware::{
    connection::{connection, net_task_xdp},
    sta,
};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);
    info!("[Init][Embassy] Embassy initialized!");

    patch::map::init();
    info!("[Init][MAP] MAP resource pool initialized");

    #[cfg(all(feature = "hw-selftest", feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    match patch::hwtest::verify_pool().await {
        Ok(()) => info!("[Init][MAP] talc resource pool verification passed"),
        Err(e) => error!("[Init][MAP] talc resource pool verification failed: {}", e),
    }
    #[cfg(all(feature = "hw-selftest", feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
    match patch::hwtest::verify_pool() {
        Ok(()) => info!("[Init][MAP] talc resource pool verification passed"),
        Err(e) => error!("[Init][MAP] talc resource pool verification failed: {}", e),
    }

    #[cfg(all(feature = "patch-sync-embassy", not(feature = "patch-sync-st")))]
    patch::map::init_firmware_maps()
        .await
        .expect("init_firmware_maps: talc pool OOM for bpf.toml maps");
    #[cfg(all(feature = "patch-sync-st", not(feature = "patch-sync-embassy")))]
    patch::map::init_firmware_maps()
        .expect("init_firmware_maps: talc pool OOM for bpf.toml maps");
    info!("firmware maps from bpf.toml initialized");

    info!("Read partition table / load XDP payload");
    patch::load(peripherals.FLASH);
    // TEMP: exercise AOT without WiFi RX (maps_helpers → bpf_printk flood).
    let _ = patch::smoke_xdp();
    patch::smoke_multi_ctx();

    #[cfg(feature = "hw-selftest")]
    {
        let _ = spawner;
        patch::hwtest::run();
        // Dedicated harness owns XDP; do not bring up WiFi / RX path.
        info!("[HWTEST] idle (WiFi skipped)");
        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
        }
    }

    #[cfg(not(feature = "hw-selftest"))]
    {
        let (controller, sta_stack, sta_runner) = sta::init_sta(peripherals.WIFI);

        spawner.spawn(connection(controller).unwrap());
        spawner.spawn(net_task_xdp(sta_runner).unwrap());

        sta::handler(sta_stack).await;
    }

    // for inspiration have a look at the examples at https://github.com/esp-rs/esp-hal/tree/esp-hal-v1.1.0/examples
}
