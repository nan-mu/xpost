#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::ptr::addr_of_mut;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info};
use xpost::map::{HEAP_SIZE, MapRt};
use xpost::xdp::{ReportItem, Slot, XdpRt};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Map talc heap — size must match `[package.metadata.xpost.map].heap_size`.
#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Single RX slot (firmware-owned). Symbol name matches xpost-build `SLOT_SYMBOL`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".bss")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let flash = peripherals.FLASH;

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    // `main -> !` cannot return `Result`; boot-fatal errors panic into our handler.
    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap)
        .await
        .expect("MapRt::init");

    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }];

    let (rt, report) = XdpRt::init(flash, &slots, just_drop::mem_map::TABLE, &mut maps)
        .await
        .expect("XdpRt::init");
    info!(
        "xdp init ok images={} write-bin {:#x} <xdp.elf>",
        rt.image_count(),
        just_drop::mem_map::TABLE.0
    );
    for item in &report {
        match item {
            ReportItem::Xdp(name) => info!("report xdp {name}"),
            ReportItem::Map(ty, name) => info!("report map {ty:?} {name}"),
        }
    }

    let (mut _wifi_controller, _interfaces) =
        esp_radio::wifi::new(peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    let _ = spawner;

    loop {
        info!("just_drop idle");
        Timer::after(Duration::from_secs(1)).await;
    }
}
