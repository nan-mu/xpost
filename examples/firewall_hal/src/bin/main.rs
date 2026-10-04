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
use portable_atomic::{AtomicU32, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use firewall_hal::map_crud::{
    self, MapCrudAck, STATUS_BAD, STATUS_CONFLICT, STATUS_NOT_FOUND, STATUS_OK,
};
use firewall_hal::wire;
use heapless::FnvIndexMap;
use log::{error, info};
use static_cell::StaticCell;
#[allow(unused_imports)] // PerfError only when perf-cag is off
use xpost_serial::reply::{MapError, OkReply, PerfError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

#[cfg(feature = "perf-cag")]
use firewall_hal::cag;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const STIM_PORT: u16 = 8080;
const CTRL_PORT: u16 = 8081;
const REPLY_PORT: u16 = 18080;
const MAP_BLACKLIST: &str = "blacklist";
const MAX_BLACKLIST: usize = 256;

static NET_RX: AtomicUsize = AtomicUsize::new(0);
static DROP: AtomicUsize = AtomicUsize::new(0);
static ACK_IP: AtomicU32 = AtomicU32::new(0);
static ACK_PORT: AtomicU32 = AtomicU32::new(0);

type BlacklistMap = FnvIndexMap<u32, u8, MAX_BLACKLIST>;
static BLACKLIST: critical_section::Mutex<core::cell::RefCell<BlacklistMap>> =
    critical_section::Mutex::new(core::cell::RefCell::new(FnvIndexMap::new()));

static NET_RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
static UDP_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static UDP_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static UDP_RX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static UDP_TX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static CTRL_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_RX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static CTRL_TX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();

#[allow(clippy::large_stack_frames, reason = "main allocates sockets / wifi")]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Error);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();

    spawner.spawn(control_serial(usb_rx).unwrap());
    esp_println::println!("xpost ready ipv4=? port={STIM_PORT}");

    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false);
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
            spawner.spawn(udp_task(stack).unwrap());
            spawner.spawn(ctrl_task(stack).unwrap());
            spawner.spawn(wifi_task(controller).unwrap());
            info!("sta ssid={ssid} stim={STIM_PORT} ctrl={CTRL_PORT}");
        }
        _ => info!("WIFI unset"),
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
        Timer::after(Duration::from_secs(2)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface<'static>>) {
    runner.run().await
}

fn ip_key_from_addr(addr: embassy_net::IpAddress) -> Option<u32> {
    let embassy_net::IpAddress::Ipv4(v4) = addr;
    Some(u32::from_be_bytes(v4.octets()))
}

fn is_blocked(key: u32) -> bool {
    critical_section::with(|cs| {
        let map = BLACKLIST.borrow(cs).borrow();
        matches!(map.get(&key), Some(&v) if v != 0)
    })
}

#[embassy_executor::task]
#[allow(clippy::large_stack_frames, reason = "UDP socket")]
async fn udp_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        esp_println::println!("xpost ready ipv4={} port={STIM_PORT}", v4.address);
    }
    let rx_meta = UDP_RX_META.init([PacketMetadata::EMPTY; 4]);
    let tx_meta = UDP_TX_META.init([PacketMetadata::EMPTY; 4]);
    let rx_buf = UDP_RX_BUF.init([0; 1536]);
    let tx_buf = UDP_TX_BUF.init([0; 1536]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(STIM_PORT).expect("bind stim");
    let mut buf = [0u8; 256];
    loop {
        let Ok((_n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        let Some(key) = ip_key_from_addr(meta.endpoint.addr) else {
            continue;
        };
        if is_blocked(key) {
            DROP.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        NET_RX.fetch_add(1, Ordering::Relaxed);
        let dest = ack_endpoint(meta.endpoint.addr, meta.endpoint.port);
        let _ = socket.send_to(b"ACK\n", dest).await;
    }
}

#[embassy_executor::task]
#[allow(clippy::large_stack_frames, reason = "MapCrud ctrl")]
async fn ctrl_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    let rx_meta = CTRL_RX_META.init([PacketMetadata::EMPTY; 4]);
    let tx_meta = CTRL_TX_META.init([PacketMetadata::EMPTY; 4]);
    let rx_buf = CTRL_RX_BUF.init([0; 1536]);
    let tx_buf = CTRL_TX_BUF.init([0; 1536]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(CTRL_PORT).expect("bind ctrl");
    let mut buf = [0u8; 512];
    let mut ack_buf = [0u8; 256];
    let mut val_buf = [0u8; 8];
    loop {
        let Ok((n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        let (status, kind, value) = handle_map_crud(&buf[..n], &mut val_buf);
        let ack = MapCrudAck {
            kind,
            status,
            value,
            reason: "",
        };
        let Some(alen) = map_crud::encode_ack(&ack, &mut ack_buf) else {
            continue;
        };
        let _ = socket
            .send_to(&ack_buf[..alen], (meta.endpoint.addr, REPLY_PORT))
            .await;
    }
}

fn handle_map_crud<'a>(pkt: &[u8], val_buf: &'a mut [u8; 8]) -> (u32, u32, &'a [u8]) {
    let Some(op) = map_crud::decode_op(pkt) else {
        return (STATUS_BAD, 0, &[]);
    };
    let kind = op.kind;
    if op.map_name != MAP_BLACKLIST {
        return (STATUS_NOT_FOUND, kind, &[]);
    }
    match op.kind {
        map_crud::KIND_CREATE => (STATUS_CONFLICT, kind, &[]),
        map_crud::KIND_DESTROY_MAP => (STATUS_BAD, kind, &[]),
        map_crud::KIND_LOOKUP => {
            if op.key.len() != 4 {
                return (STATUS_BAD, kind, &val_buf[..0]);
            }
            let key = u32::from_be_bytes([op.key[0], op.key[1], op.key[2], op.key[3]]);
            critical_section::with(|cs| {
                let map = BLACKLIST.borrow(cs).borrow();
                match map.get(&key).copied() {
                    Some(v) => {
                        val_buf[0] = v;
                        (STATUS_OK, kind, &val_buf[..1])
                    }
                    None => (STATUS_NOT_FOUND, kind, &val_buf[..0]),
                }
            })
        }
        map_crud::KIND_UPDATE => {
            if op.key.len() != 4 {
                return (STATUS_BAD, kind, &val_buf[..0]);
            }
            let key = u32::from_be_bytes([op.key[0], op.key[1], op.key[2], op.key[3]]);
            let val = if op.value.is_empty() { 1 } else { op.value[0] };
            critical_section::with(|cs| {
                let mut map = BLACKLIST.borrow(cs).borrow_mut();
                match map.insert(key, val) {
                    Ok(_) => (STATUS_OK, kind, &val_buf[..0]),
                    Err(_) => (STATUS_BAD, kind, &val_buf[..0]),
                }
            })
        }
        map_crud::KIND_DELETE => {
            if op.key.len() != 4 {
                return (STATUS_BAD, kind, &val_buf[..0]);
            }
            let key = u32::from_be_bytes([op.key[0], op.key[1], op.key[2], op.key[3]]);
            critical_section::with(|cs| {
                let mut map = BLACKLIST.borrow(cs).borrow_mut();
                if map.remove(&key).is_some() {
                    (STATUS_OK, kind, &val_buf[..0])
                } else {
                    (STATUS_NOT_FOUND, kind, &val_buf[..0])
                }
            })
        }
        _ => (STATUS_BAD, kind, &[]),
    }
}

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
        .on_enable_xdp(|reply: ReplyWriter<'_>| reply.ok(OkReply::EnableXdp))
        .on_disable_xdp(|reply: ReplyWriter<'_>| reply.ok(OkReply::DisableXdp))
        .on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
            if name != MAP_BLACKLIST {
                reply.error(&MapError::NameNotFound);
                return;
            }
            critical_section::with(|cs| BLACKLIST.borrow(cs).borrow_mut().clear());
            reply.ok(OkReply::ClearMap);
        })
        .on_get_map(|name: &str, reply: ReplyWriter<'_>| {
            if name != MAP_BLACKLIST {
                reply.error(&MapError::NameNotFound);
                return;
            }
            critical_section::with(|cs| {
                let map = BLACKLIST.borrow(cs).borrow();
                let size = map.len() as u32;
                let cap = MAX_BLACKLIST as u32;
                let mut len = wire::hash_header_len(size, cap);
                for (k, v) in map.iter() {
                    len += wire::hash_entry_len(&k.to_be_bytes(), &[*v]);
                }
                reply.get_map(len, |write| {
                    let mut emit = |chunk: &[u8]| {
                        let _ = write(chunk);
                    };
                    wire::write_hash_header(size, cap, &mut emit);
                    for (k, v) in map.iter() {
                        wire::write_hash_entry(&k.to_be_bytes(), &[*v], &mut emit);
                    }
                });
            });
        })
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                if name != MAP_BLACKLIST {
                    reply.error(&MapError::NameNotFound);
                    return;
                }
                let Some(ip) = wire::parse_ipv4_token(key) else {
                    reply.error(&MapError::InvalidKey);
                    return;
                };
                let Some(val) = wire::parse_u64_token(value) else {
                    reply.error(&MapError::InvalidValue);
                    return;
                };
                if val > u64::from(u8::MAX) {
                    reply.error(&MapError::InvalidValue);
                    return;
                }
                let k = u32::from_be_bytes(ip);
                let ok = critical_section::with(|cs| {
                    BLACKLIST.borrow(cs).borrow_mut().insert(k, val as u8).is_ok()
                });
                if ok {
                    reply.ok(OkReply::UpdateMap);
                } else {
                    reply.error(&MapError::Full);
                }
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
