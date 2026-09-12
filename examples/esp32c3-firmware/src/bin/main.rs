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
use esp32c3_firmware::init_log;
use esp32c3_firmware::log_gate::init_logger;

#[cfg(not(fw_us_hw_selftest))]
use esp32c3_firmware::{
    connection::{connection, net_task_xdp},
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

    #[cfg(fw_us_hw_selftest)]
    match esp32c3_firmware::hwtest::verify_pool().await {
        Ok(()) => init_log!("[Init][MAP] talc resource pool verification passed"),
        Err(e) => error!("[Init][MAP] talc resource pool verification failed: {}", e),
    }

    #[cfg(not(fw_us_cold_boot))]
    {
        maps.from_bpf_toml()
            .await
            .expect("[Init][MAP] talc pool OOM for bpf.toml maps");
        init_log!("[Init][MAP] firmware maps from bpf.toml initialized");

        init_log!("[Init][XDP] load AOT payload into leased slot");
        let mut xdp = xpost::xdp::XdpRt::init(&XDP_SLOT)
            .await
            .expect("[Init][XDP] runtime init failed");
        let id = xdp
            .push(
                "primary",
                include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/esp32c3.bin")),
            )
            .expect("[Init][XDP] image too large");
        xdp.r#move(id, xpost::xdp::XdpLocation::Slot { index: 0 })
            .expect("[Init][XDP] move to primary");
        xdp.lease(Some(&mut maps))
            .expect("[Init][XDP] map/image lease failed");
        drop(xdp);
        drop(maps);
    }
    #[cfg(fw_us_cold_boot)]
    {
        init_log!("[Init][MAP] cold_boot: skip bpf.toml maps (CREATE via control plane)");
        init_log!("[Init][XDP] cold_boot: no leased image; RX remains XDP_PASS");
        let _ = xpost::xdp::XdpRt::init(&XDP_SLOT)
            .await
            .expect("[Init][XDP] runtime init failed");
        drop(maps);
        let _ = peripherals.FLASH;
    }

    #[cfg(fw_us_hw_selftest)]
    {
        let _ = spawner;
        esp32c3_firmware::hwtest::run();
        init_log!("[HWTEST] idle (WiFi skipped)");
        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
        }
    }

    #[cfg(not(fw_us_hw_selftest))]
    {
        #[cfg(any(
            fw_us_vip_seed,
            fw_us_firewall_seed,
            fw_us_fw_rules_seed,
            fw_us_firewall_array_seed,
        ))]
        esp32c3_firmware::fw_seed::run_seeds().await;

        let (controller, sta_stack, sta_runner) = sta::init_sta(peripherals.WIFI);

        spawner.spawn(connection(controller).unwrap());
        spawner.spawn(net_task_xdp(sta_runner).unwrap());

        sta::start_udp_tasks(spawner, sta_stack);
        loop {
            embassy_time::Timer::after(embassy_time::Duration::from_secs(60)).await;
        }
    }
}
