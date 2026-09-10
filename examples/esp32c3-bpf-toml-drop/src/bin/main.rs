#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp32c3_bpf_toml_drop::{
    connection::{connection, net_task},
    init_log,
    log_gate::init_logger,
    sta,
};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("PANIC {panic_info}");
    esp_hal::system::software_reset();
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const MAP_HEAP_SIZE: usize = xpost::map::HEAP_SIZE;
const XDP_SLOT_LEN: usize = xpost::mem_map::HOT_A_LEN;

#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; MAP_HEAP_SIZE] = [0; MAP_HEAP_SIZE];

#[unsafe(no_mangle)]
#[cfg_attr(target_arch = "riscv32", unsafe(link_section = ".xdp.hotload_0"))]
static XDP_SLOT: xpost::xdp::Slot<XDP_SLOT_LEN> = xpost::xdp::Slot::new([0; XDP_SLOT_LEN]);

#[allow(clippy::large_stack_frames)]
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

    let heap = unsafe {
        core::slice::from_raw_parts_mut(
            core::ptr::addr_of_mut!(MAP_HEAP).cast::<u8>(),
            MAP_HEAP_SIZE,
        )
    };
    let mut maps = xpost::map::MapRt::init(heap)
        .await
        .expect("[Init][MAP] map runtime init failed");
    init_log!("[Init][MAP] MAP resource pool initialized");

    maps.from_bpf_toml()
        .await
        .expect("[Init][MAP] bpf.toml maps failed");
    init_log!("[Init][MAP] firmware maps from bpf.toml initialized");

    let mut xdp = xpost::xdp::XdpRt::init(&XDP_SLOT)
        .await
        .expect("[Init][XDP] runtime init failed");
    xdp.from_bpf_toml()
        .expect("[Init][XDP] bpf.toml images failed");
    xdp.lease(Some(&mut maps))
        .expect("[Init][XDP] lease failed");
    drop(xdp);
    drop(maps);
    init_log!("[Init][XDP] leased from bpf.toml");

    let (controller, _sta_stack, sta_runner) = sta::init_sta(peripherals.WIFI);
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(sta_runner).unwrap());

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
