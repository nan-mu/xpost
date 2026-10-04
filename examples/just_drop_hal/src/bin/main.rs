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
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use just_drop_hal::wire;
use log::{error, info};
use static_cell::StaticCell;
#[allow(unused_imports)] // PerfError only when perf-cag is off
use xpost_serial::reply::{MapError, OkReply, PerfError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

#[cfg(feature = "perf-cag")]
use just_drop_hal::cag;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// UDP port the netstack accepts (same stim surface as just_drop).
const STIM_PORT: u16 = 8080;

/// Packets that reached the UDP socket in Pass mode (`drop off`).
static NET_RX: AtomicUsize = AtomicUsize::new(0);
/// Stim datagrams discarded in Drop mode — primary `drop=` for formal.
static DROP: AtomicUsize = AtomicUsize::new(0);
/// Native array mirror of XDP `counter` map: key = IPv4 src last octet.
static COUNTER: [AtomicU64; 256] = [const { AtomicU64::new(0) }; 256];
/// When true, stim UDP is counted as drop and not ACKed. Formal defaults armed.
static DROP_ARMED: AtomicBool = AtomicBool::new(true);
/// Optional fixed ACK destination (host order). Zero address → reply to the sender.
static ACK_IP: AtomicU32 = AtomicU32::new(0);
static ACK_PORT: AtomicU32 = AtomicU32::new(0);

static NET_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
static UDP_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static UDP_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static UDP_RX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static UDP_TX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();

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
    // Wi-Fi dynamic RX/TX buffers; align with just_drop / just_tx_hal.
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();

    // App serial after board init; emits unsolicited `OK init`.
    spawner.spawn(control_serial(usb_rx).unwrap());
    esp_println::println!("xpost ready ipv4=? port={STIM_PORT}");

    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false);
    let (mut controller, interfaces) = esp_radio::wifi::new(peripherals.WIFI, wifi_cfg)
        .expect("Failed to initialize Wi-Fi controller");
    info!("wifi drop_site=app tx_dyn=128 rx_dyn=1024");

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
            spawner.spawn(udp_task(stack).unwrap());
            spawner.spawn(wifi_task(controller).unwrap());
            info!("sta ssid={ssid} stim_port={STIM_PORT}");
        }
        _ => {
            info!("WIFI_SSID/WIFI_PASS unset; UDP ack disabled, serial counters still live");
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
            Ok(_) => info!("wifi connected"),
            Err(e) => {
                error!("wifi connect: {e:?}");
                Timer::after(Duration::from_secs(2)).await;
            }
        }
        let _ = controller.wait_for_disconnect_async().await;
        info!("wifi disconnected");
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
    reason = "embassy UdpSocket future and RX scratch buffer"
)]
async fn udp_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        info!("dhcp {}", v4.address);
        esp_println::println!("xpost ready ipv4={} port={STIM_PORT}", v4.address);
    }

    let rx_meta = UDP_RX_META.init([PacketMetadata::EMPTY; 4]);
    let tx_meta = UDP_TX_META.init([PacketMetadata::EMPTY; 4]);
    let rx_buf = UDP_RX_BUF.init([0; 1536]);
    let tx_buf = UDP_TX_BUF.init([0; 1536]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(STIM_PORT).expect("bind stim");
    info!("udp ack listening {STIM_PORT}");

    let mut buf = [0u8; 256];
    loop {
        let Ok((n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if let Some(octet) = ipv4_last_octet(meta.endpoint.addr) {
            COUNTER[octet as usize].fetch_add(1, Ordering::Relaxed);
        }

        if DROP_ARMED.load(Ordering::Relaxed) {
            DROP.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        NET_RX.fetch_add(1, Ordering::Relaxed);
        let dest = ack_endpoint(meta.endpoint.addr, meta.endpoint.port);
        let payload = b"ACK\n";
        if socket.send_to(payload, dest).await.is_err() {
            error!("ack send failed bytes={n}");
        }
    }
}

const COUNTER_SIZE: u32 = 256;

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
        .on_enable_xdp(|reply: ReplyWriter<'_>| {
            DROP_ARMED.store(true, Ordering::Relaxed);
            reply.ok(OkReply::EnableXdp);
        })
        .on_disable_xdp(|reply: ReplyWriter<'_>| {
            DROP_ARMED.store(false, Ordering::Relaxed);
            reply.ok(OkReply::DisableXdp);
        })
        .on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
            if !name.eq_ignore_ascii_case("counter") {
                reply.error(&MapError::NameNotFound);
                return;
            }
            for c in COUNTER.iter() {
                c.store(0, Ordering::Relaxed);
            }
            reply.ok(OkReply::ClearMap);
        })
        .on_get_map(|name: &str, reply: ReplyWriter<'_>| {
            if !name.eq_ignore_ascii_case("counter") {
                reply.error(&MapError::NameNotFound);
                return;
            }
            let len = wire::array_blob_len(COUNTER_SIZE, |i| {
                COUNTER[i as usize].load(Ordering::Relaxed)
            });
            reply.get_map(len, |write| {
                wire::write_array_blob(
                    COUNTER_SIZE,
                    |i| COUNTER[i as usize].load(Ordering::Relaxed),
                    &mut |chunk| {
                        let _ = write(chunk);
                    },
                );
            });
        })
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                if !name.eq_ignore_ascii_case("counter") {
                    reply.error(&MapError::NameNotFound);
                    return;
                }
                let Some(slot) = wire::parse_u32_token(key) else {
                    reply.error(&MapError::InvalidKey);
                    return;
                };
                if slot >= COUNTER_SIZE {
                    reply.error(&MapError::InvalidKey);
                    return;
                }
                let Some(val) = wire::parse_u64_token(value) else {
                    reply.error(&MapError::InvalidValue);
                    return;
                };
                COUNTER[slot as usize].store(val, Ordering::Relaxed);
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

fn ipv4_last_octet(addr: embassy_net::IpAddress) -> Option<u8> {
    let embassy_net::IpAddress::Ipv4(v4) = addr;
    Some(v4.octets()[3])
}
