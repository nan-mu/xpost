#![no_std]
#![no_main]
#![deny(clippy::mem_forget)]
#![deny(clippy::large_stack_frames)]

use embassy_executor::Spawner;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp32c3_direct::{
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
#[unsafe(link_section = ".xdp.hotload_0")]
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
    let maps = xpost::map::MapRt::init(heap)
        .await
        .expect("[Init][MAP] map runtime init failed");
    init_log!("[Init][MAP] MAP resource pool initialized");
    // Direct-start: no MapBuilder::from_bpf_toml / maps.from_bpf_toml.
    drop(maps);

    let mut xdp = xpost::xdp::XdpRt::init(&XDP_SLOT)
        .await
        .expect("[Init][XDP] runtime init failed");
    xdp.assume_injected()
        .expect("[Init][XDP] assume_injected failed");
    xdp.lease(None).expect("[Init][XDP] lease(None) failed");
    drop(xdp);
    init_log!("[Init][XDP] direct-start leased (injected slot)");

    let (controller, _sta_stack, sta_runner) = sta::init_sta(peripherals.WIFI);
    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task(sta_runner).unwrap());

    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
