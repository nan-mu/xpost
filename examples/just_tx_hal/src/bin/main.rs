#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::convert::Infallible;

use alloc::string::ToString;
use portable_atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use just_tx_hal::wire;
use log::error;
use static_cell::StaticCell;
#[allow(unused_imports)] // PerfError only when perf-cag is off
use xpost_serial::reply::{MapError, OkReply, PerfError, ServerError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

#[cfg(feature = "perf-cag")]
use just_tx_hal::cag;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// UDP port the netstack accepts (same stim surface as just_drop / just_tx).
const STIM_PORT: u16 = 8080;
/// IPPROTO_UDP — matches just_tx `rxcnt` ARRAY key.
const IPPROTO_UDP: u32 = 17;

/// Packets that reached the UDP socket (always, before echo).
static NET_RX: AtomicUsize = AtomicUsize::new(0);
/// Per-ipproto counters; UDP (17) increments on recv (aligns with XDP just_tx rxcnt).
static RXCNT: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
/// Optional fixed ACK destination (host order). Zero address → reply to the sender.
static ACK_IP: AtomicU32 = AtomicU32::new(0);
static ACK_PORT: AtomicU32 = AtomicU32::new(0);

static NET_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
static UDP_RX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static UDP_TX_META: StaticCell<[PacketMetadata; 16]> = StaticCell::new();
static UDP_RX_BUF: StaticCell<[u8; 4096]> = StaticCell::new();
static UDP_TX_BUF: StaticCell<[u8; 4096]> = StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // Quiet USB: match just_drop under stress (agent session contention).
    esp_println::logger::init_logger(log::LevelFilter::Error);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);
    // Wi-Fi dynamic RX/TX buffers; align with just_drop / just_drop_hal.
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();

    // AMPDU TX off: its BA session stalls TX ~1.07 s under ping-pong load.
    // Embassy frame queues (HAL-only): defaults 5/3 drop under burst.
    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false)
        .with_rx_queue_size(32)
        .with_tx_queue_size(16);
    let (mut controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, wifi_cfg)
        .expect("Failed to initialize Wi-Fi controller");

    match (option_env!("WIFI_SSID"), option_env!("WIFI_PASS")) {
        (Some(ssid), Some(pass)) if !ssid.is_empty() => {
            let station = esp_radio::wifi::Config::Station(
                esp_radio::wifi::sta::StationConfig::default()
                    .with_ssid(ssid)
                    .with_password(pass.to_string()),
            );
            controller.set_config(&station).expect("wifi set_config");
            let seed = 0xC3C3_5A5A;
            let resources = NET_RESOURCES.init(StackResources::new());
            let net_config = embassy_net::Config::dhcpv4(Default::default());
            let (stack, runner) = embassy_net::new(interfaces.station, net_config, resources, seed);
            spawner.spawn(net_task(runner).unwrap());
            spawner.spawn(udp_task(stack, usb_rx, spawner).unwrap());
            spawner.spawn(wifi_task(controller).unwrap());
        }
        _ => {
            spawner.spawn(control_serial(usb_rx).unwrap());
        }
    }

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[embassy_executor::task]
async fn wifi_task(mut controller: esp_radio::wifi::WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(_) => {}
            Err(e) => {
                error!("wifi connect: {e:?}");
                Timer::after(Duration::from_secs(2)).await;
            }
        }
        let _ = controller.wait_for_disconnect_async().await;
        Timer::after(Duration::from_secs(2)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface<'static>>) {
    runner.run().await
}

#[embassy_executor::task]
#[allow(
    clippy::large_stack_frames,
    reason = "embassy UdpSocket future and larger TX/RX scratch buffers"
)]
async fn udp_task(
    stack: embassy_net::Stack<'static>,
    usb_rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
    spawner: Spawner,
) {
    stack.wait_config_up().await;

    let rx_meta = UDP_RX_META.init([PacketMetadata::EMPTY; 16]);
    let tx_meta = UDP_TX_META.init([PacketMetadata::EMPTY; 16]);
    let rx_buf = UDP_RX_BUF.init([0; 4096]);
    let tx_buf = UDP_TX_BUF.init([0; 4096]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(STIM_PORT).expect("bind stim");
    // `OK init` after DHCP + bind: GetInit means dataplane is live.
    spawner.spawn(control_serial(usb_rx).unwrap());

    let mut buf = [0u8; 256];
    loop {
        let Ok((n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        #[cfg(feature = "perf-cag")]
        let t0 = cag::now_ticks();
        #[cfg(feature = "perf-cag")]
        {
            cag::bump_rx_packets();
            cag::bump_xdp_invocations();
            cag::set_rx_queue_depth(1);
        }
        NET_RX.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "perf-cag")]
        let t_map = cag::now_ticks();
        RXCNT[IPPROTO_UDP as usize].fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "perf-cag")]
        {
            cag::bump_helper_map();
            let t1 = cag::now_ticks();
            cag::record_dispatch(t_map.saturating_sub(t0));
            cag::record_map_operation(t1.saturating_sub(t_map));
            cag::record_execution(t1.saturating_sub(t_map));
        }
        let dest = ack_endpoint(meta.endpoint.addr, meta.endpoint.port);
        #[cfg(feature = "perf-cag")]
        {
            cag::bump_xdp_tx();
            cag::bump_rx_delivered();
            cag::bump_tx_submitted();
            cag::set_tx_inflight(1);
            cag::note_block_start();
        }
        #[cfg(feature = "perf-cag")]
        let t_tx = cag::now_ticks();
        let send_ok = socket.send_to(&buf[..n], dest).await.is_ok();
        #[cfg(feature = "perf-cag")]
        {
            let _ = cag::note_block_end();
            let now = cag::now_ticks();
            cag::record_post_action(now.saturating_sub(t_tx));
            cag::record_tx_submission(now.saturating_sub(t_tx));
            if send_ok {
                cag::bump_tx_completed();
            } else {
                cag::bump_tx_failed();
                cag::bump_runtime_errors();
            }
            cag::set_tx_inflight(0);
            cag::set_rx_queue_depth(0);
            cag::record_rx_processing(now.saturating_sub(t0));
        }
        if !send_ok {
            error!("echo send failed bytes={n}");
        }
    }
}

const RXCNT_SIZE: u32 = 256;

serial_task! {
    fn control_serial(
        rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
    ) {
        let mut rx = rx;
        SerialHandler::builder(
            move || match rx.read_byte() {
                Ok(b) => Some(b),
                Err(_) => None,
            },
            |bytes| {
                esp_println::Printer::write_bytes(bytes);
                Ok::<(), Infallible>(())
            },
            [0u8; 128],
        )
        .on_ping(|reply: ReplyWriter<'_>| reply.ok(OkReply::Ping))
        .on_enable_xdp(|reply: ReplyWriter<'_>| reply.error(&ServerError::Unsupported))
        .on_disable_xdp(|reply: ReplyWriter<'_>| reply.error(&ServerError::Unsupported))
        .on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
            if !name.eq_ignore_ascii_case("rxcnt") {
                reply.error(&MapError::NameNotFound);
                return;
            }
            for c in RXCNT.iter() {
                c.store(0, Ordering::Relaxed);
            }
            reply.ok(OkReply::ClearMap);
        })
        .on_get_map(|name: &str, reply: ReplyWriter<'_>| {
            if !name.eq_ignore_ascii_case("rxcnt") {
                reply.error(&MapError::NameNotFound);
                return;
            }
            let len = wire::array_blob_len(RXCNT_SIZE, |i| {
                RXCNT[i as usize].load(Ordering::Relaxed)
            });
            reply.get_map(len, |write| {
                wire::write_array_blob(
                    RXCNT_SIZE,
                    |i| RXCNT[i as usize].load(Ordering::Relaxed),
                    &mut |chunk| {
                        let _ = write(chunk);
                    },
                );
            });
        })
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                if !name.eq_ignore_ascii_case("rxcnt") {
                    reply.error(&MapError::NameNotFound);
                    return;
                }
                let Some(slot) = wire::parse_u32_token(key) else {
                    reply.error(&MapError::InvalidKey);
                    return;
                };
                if slot >= RXCNT_SIZE {
                    reply.error(&MapError::InvalidKey);
                    return;
                }
                let Some(val) = wire::parse_u64_token(value) else {
                    reply.error(&MapError::InvalidValue);
                    return;
                };
                RXCNT[slot as usize].store(val, Ordering::Relaxed);
                reply.ok(OkReply::UpdateMap);
            },
        )
        .on_clear_perf(|reply: ReplyWriter<'_>| {
            #[cfg(feature = "perf-cag")]
            {
                cag::clear_perf();
                reply.ok(OkReply::ClearPerf);
            }
            #[cfg(not(feature = "perf-cag"))]
            {
                reply.error(&PerfError::Unavailable);
            }
        })
        .on_get_perf(|reply: ReplyWriter<'_>| {
            #[cfg(feature = "perf-cag")]
            {
                let n = cag::report_payload_len();
                reply.get_perf(n, |write| {
                    cag::report_perf(write);
                });
            }
            #[cfg(not(feature = "perf-cag"))]
            {
                reply.error(&PerfError::Unavailable);
            }
        })
        .build()
    }
}

/// Fixed `ack-dest` when set; otherwise the stimulus source (the agent socket).
fn ack_endpoint(src: embassy_net::IpAddress, src_port: u16) -> (embassy_net::IpAddress, u16) {
    let ip = ACK_IP.load(Ordering::Relaxed);
    let port = ACK_PORT.load(Ordering::Relaxed) as u16;
    if ip != 0 && port != 0 {
        let o = ip.to_be_bytes();
        (embassy_net::IpAddress::v4(o[0], o[1], o[2], o[3]), port)
    } else {
        (src, src_port)
    }
}
