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
use esp32c3_drop_xdp::{
    connection::{connection, net_task_xdp},
    evt_async,
    init_log,
    log_gate::init_logger,
    sta,
};
use xpost::map::{MapBuilder, MapIdxExt, claim_heap, init, init_firmware_maps, map_rt};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    esp_println::println!("PANIC {panic_info}");
    esp_hal::system::software_reset();
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const MAP_HEAP_SIZE: usize = xpost::map::HEAP_SIZE;
const XDP_SLOT_LEN: usize = xpost::mem_map::HOT_A_LEN;

#[link_section = ".bss"]
static mut MAP_HEAP: [u8; MAP_HEAP_SIZE] = [0; MAP_HEAP_SIZE];

#[unsafe(no_mangle)]
#[cfg_attr(target_arch = "riscv32", unsafe(link_section = ".xdp.hotload_0"))]
static XDP_SLOT: xpost::xdp::Slot<XDP_SLOT_LEN> = xpost::xdp::Slot::new([0; XDP_SLOT_LEN]);

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

    unsafe {
        claim_heap(&mut MAP_HEAP).expect("[Init][MAP] claim_heap failed");
    }
    init().await.expect("[Init][MAP] map runtime init failed");
    init_log!("[Init][MAP] MAP resource pool initialized");
    init_firmware_maps()
        .await
        .expect("[Init][MAP] talc pool OOM for bpf.toml maps");
    init_log!("[Init][MAP] firmware maps from bpf.toml initialized");

    let mut rt = map_rt().await.expect("[Init][MAP] map_rt");
    let dut_evt = rt
        .push_builder(
            MapBuilder::ringbuf()
                .with_name("dut_evt")
                .with_capacity(4096),
        )
        .await
        .expect("[Init][MAP] dut_evt ringbuf OOM");
    drop(rt);

    esp32c3_drop_xdp::drop_stats::install_evt_handlers();
    let rx = dut_evt
        .into_channel()
        .await
        .expect("[Init][MAP] dut_evt into_channel");
    spawner
        .spawn(evt_async::run(rx))
        .expect("dut_evt consumer");

    init_log!("[Init][XDP] register, load, and lease AOT slot");
    let mut xdp_rt = xpost::xdp::XdpRt::init(&XDP_SLOT)
        .await
        .expect("[Init][XDP] runtime init failed");
    xdp_rt.load_primary(include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/esp32c3.bin"
    )))
    .expect("[Init][XDP] image too large");
    let maps = map_rt().await.expect("[Init][MAP] map_rt lease");
    xdp_rt
        .lease(Some(&maps))
        .await
        .expect("[Init][XDP] map/image lease failed");
    drop(maps);
    drop(xdp_rt);

    let (controller, sta_stack, sta_runner) = sta::init_sta(peripherals.WIFI);

    spawner.spawn(connection(controller).unwrap());
    spawner.spawn(net_task_xdp(sta_runner).unwrap());

    sta::start_udp_tasks(spawner, sta_stack);
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
    }
}
