#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::string::ToString;
use portable_atomic::{AtomicU32, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use heapless::FnvIndexMap;
use log::{error, info};
use static_cell::StaticCell;
use xpost::xdp::Slot;

use firewall_hal::map_crud::{
    self, MapCrudAck, STATUS_BAD, STATUS_CONFLICT, STATUS_NOT_FOUND, STATUS_OK,
};

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

#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

static NET_RX: AtomicUsize = AtomicUsize::new(0);
static DROP: AtomicUsize = AtomicUsize::new(0);
static ACK_IP: AtomicU32 = AtomicU32::new(0);
static ACK_PORT: AtomicU32 = AtomicU32::new(0);

type BlacklistMap = FnvIndexMap<u32, u64, MAX_BLACKLIST>;
static BLACKLIST: critical_section::Mutex<core::cell::RefCell<BlacklistMap>> =
    critical_section::Mutex::new(core::cell::RefCell::new(FnvIndexMap::new()));
/// When false, stim path ignores packets (agent `xdp off` / `disable xdp`).
static DATAPLANE: portable_atomic::AtomicBool = portable_atomic::AtomicBool::new(true);

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
    spawner.spawn(serial_task(usb_rx).unwrap());
    Timer::after(Duration::from_millis(1200)).await;
    esp_println::println!("xpost ready ipv4=? port={STIM_PORT}");

    let _slot = core::ptr::addr_of!(xdp_slot);

    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false);
    let (mut controller, interfaces) = xpost::esp_radio::new(peripherals.WIFI, wifi_cfg)
        .expect("Failed to initialize Wi-Fi controller");

    spawner.spawn(xdp_tx_task().unwrap());

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
async fn xdp_tx_task() {
    use embassy_futures::select::select;
    use esp_radio::wifi::{XDP_TX_KICK, XdpTxPoll, xdp_tx_poll};
    loop {
        match xdp_tx_poll() {
            XdpTxPoll::Idle => XDP_TX_KICK.wait().await,
            XdpTxPoll::Blocked => {
                select(XDP_TX_KICK.wait(), Timer::after(Duration::from_micros(500))).await;
            }
        }
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

fn hit_and_count(key: u32) -> bool {
    critical_section::with(|cs| {
        let mut map = BLACKLIST.borrow(cs).borrow_mut();
        match map.get_mut(&key) {
            Some(v) => {
                *v = v.saturating_add(1);
                true
            }
            None => false,
        }
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
        if !DATAPLANE.load(Ordering::Relaxed) {
            continue;
        }
        let Some(key) = ip_key_from_addr(meta.endpoint.addr) else {
            continue;
        };
        if hit_and_count(key) {
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
                        val_buf[..8].copy_from_slice(&v.to_le_bytes());
                        (STATUS_OK, kind, &val_buf[..8])
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
            let val = if op.value.is_empty() {
                0u64
            } else {
                let mut b = [0u8; 8];
                let n = op.value.len().min(8);
                b[..n].copy_from_slice(&op.value[..n]);
                u64::from_le_bytes(b)
            };
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

#[embassy_executor::task]
async fn serial_task(
    mut rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
) {
    let mut line = [0u8; 80];
    let mut len = 0usize;
    loop {
        match rx.read_byte() {
            Ok(b) => {
                if b == b'\n' || b == b'\r' {
                    if len > 0 {
                        let text = core::str::from_utf8(&line[..len]).unwrap_or("");
                        dispatch_serial(text);
                        len = 0;
                    }
                } else if len < line.len() {
                    line[len] = b;
                    len += 1;
                }
            }
            Err(_) => Timer::after(Duration::from_millis(10)).await,
        }
    }
}

fn dispatch_serial(line: &str) {
    let line = line.trim();
    if line.eq_ignore_ascii_case("ping") {
        esp_println::println!("OK ping");
    } else if line.eq_ignore_ascii_case("report") || line.eq_ignore_ascii_case("report perf") {
        print_report();
    } else if line.eq_ignore_ascii_case("perf clear") {
        #[cfg(any(
            feature = "kind-c",
            feature = "kind-a",
            feature = "kind-g",
            feature = "kind-t"
        ))]
        xpost::ext::perf::clear_perf();
        DROP.store(0, Ordering::Relaxed);
        NET_RX.store(0, Ordering::Relaxed);
        esp_println::println!("OK perf clear");
    } else if line.eq_ignore_ascii_case("blacklist clear") {
        critical_section::with(|cs| BLACKLIST.borrow(cs).borrow_mut().clear());
        esp_println::println!("OK blacklist clear");
    } else if line.eq_ignore_ascii_case("xdp off") || line.eq_ignore_ascii_case("disable xdp") {
        DATAPLANE.store(false, Ordering::Relaxed);
        esp_println::println!("OK xdp off");
    } else if line.eq_ignore_ascii_case("xdp on") || line.eq_ignore_ascii_case("enable xdp") {
        DATAPLANE.store(true, Ordering::Relaxed);
        esp_println::println!("OK xdp on");
    } else if let Some(rest) = strip_cmd(line, "map dump") {
        print_map_dump(rest);
    } else if line.eq_ignore_ascii_case("map dump") {
        print_map_dump("");
    } else if line.eq_ignore_ascii_case("counters") {
        let drop = DROP.load(Ordering::Relaxed);
        let net_rx = NET_RX.load(Ordering::Relaxed);
        esp_println::println!("OK counters drop={drop} net_rx={net_rx}");
    } else if let Some(rest) = strip_cmd(line, "ack-dest") {
        match parse_ack_dest(rest) {
            Some((ip, port)) => {
                ACK_IP.store(ip, Ordering::Relaxed);
                ACK_PORT.store(u32::from(port), Ordering::Relaxed);
                esp_println::println!("OK ack-dest {rest}");
            }
            None => esp_println::println!("ERR ack-dest <ipv4> <port>"),
        }
    } else {
        esp_println::println!("ERR unknown");
    }
}

fn print_report() {
    #[cfg(any(
        feature = "kind-c",
        feature = "kind-a",
        feature = "kind-g",
        feature = "kind-t"
    ))]
    {
        let n = xpost::ext::perf::report_payload_len();
        esp_println::println!("OK report {n}");
        xpost::ext::perf::report_perf(|chunk| {
            esp_println::Printer::write_bytes(chunk);
            Ok::<(), core::convert::Infallible>(())
        });
    }
    #[cfg(not(any(
        feature = "kind-c",
        feature = "kind-a",
        feature = "kind-g",
        feature = "kind-t"
    )))]
    esp_println::println!("ERR report no-perf");
}

fn print_map_dump(rest: &str) {
    let name = rest.trim();
    if name.is_empty() {
        esp_println::println!("ERR map dump <name>");
        return;
    }
    if !name.eq_ignore_ascii_case(MAP_BLACKLIST) {
        esp_println::println!("ERR map not-found");
        return;
    }
    critical_section::with(|cs| {
        let map = BLACKLIST.borrow(cs).borrow();
        esp_println::println!(
            "OK map {MAP_BLACKLIST} type=hash size={} cap={MAX_BLACKLIST}",
            map.len()
        );
        for (k, v) in map.iter() {
            let o = k.to_be_bytes();
            esp_println::println!("{}.{}.{}.{} {v}", o[0], o[1], o[2], o[3]);
        }
        esp_println::println!("OK map end");
    });
}

fn strip_cmd<'a>(line: &'a str, cmd: &str) -> Option<&'a str> {
    let rest = line.get(cmd.len()..)?;
    if !line[..cmd.len()].eq_ignore_ascii_case(cmd) {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

fn parse_ack_dest(rest: &str) -> Option<(u32, u16)> {
    let mut parts = rest.split_whitespace();
    let ip = parse_v4(parts.next()?)?;
    let port: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || port == 0 {
        return None;
    }
    Some((ip, port))
}

fn parse_v4(s: &str) -> Option<u32> {
    let mut out = 0u32;
    let mut parts = 0u8;
    let mut acc = 0u32;
    let mut digits = 0u8;
    for b in s.bytes() {
        if b == b'.' {
            if digits == 0 || parts >= 3 {
                return None;
            }
            out = (out << 8) | acc;
            acc = 0;
            digits = 0;
            parts += 1;
        } else if b.is_ascii_digit() {
            digits += 1;
            if digits > 3 {
                return None;
            }
            acc = acc * 10 + u32::from(b - b'0');
            if acc > 255 {
                return None;
            }
        } else {
            return None;
        }
    }
    if parts != 3 || digits == 0 {
        return None;
    }
    Some((out << 8) | acc)
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
