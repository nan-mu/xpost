#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::ptr::addr_of_mut;

use alloc::string::ToString;
use portable_atomic::{AtomicU32, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Ipv4Address, Ipv4Cidr, StackResources, StaticConfigV4};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info};
use static_cell::StaticCell;
use xpost::map::{BPF_ANY, HEAP_SIZE, MapRt, MapUseError};
use xpost::xdp::{ReportItem, Slot, XdpRt};

use firewall::map_crud::{
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
/// Host harness binds MapCrudAck on this port (`dut.reply_port`).
const REPLY_PORT: u16 = 18080;
const MAP_BLACKLIST: &str = "blacklist";

#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

static NET_RX: AtomicUsize = AtomicUsize::new(0);
static ACK_IP: AtomicU32 = AtomicU32::new(0);
static ACK_PORT: AtomicU32 = AtomicU32::new(0);

static NET_RESOURCES: StaticCell<StackResources<5>> = StaticCell::new();
static UDP_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static UDP_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static UDP_RX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static UDP_TX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static CTRL_RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
static CTRL_RX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();
static CTRL_TX_BUF: StaticCell<[u8; 1536]> = StaticCell::new();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Error);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let flash = peripherals.FLASH;

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    spawner.spawn(serial_task(usb_rx).unwrap());
    Timer::after(Duration::from_millis(1200)).await;
    esp_println::println!(
        "xpost ready ipv4={} port={STIM_PORT}",
        option_env!("STA_IP").unwrap_or("?")
    );

    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap).await.expect("MapRt::init");
    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }];
    let (mut rt, report) = XdpRt::init(
        flash,
        &slots,
        firewall::mem_map::TABLE,
        &mut maps,
        firewall::mem_map::HELPER_DISPATCHER_BASE,
    )
    .await
    .expect("XdpRt::init");
    let n_images = report
        .iter()
        .filter(|i| matches!(i, ReportItem::Xdp(_)))
        .count();
    info!(
        "xdp init ok images={} write-bin {:#x} <xdp.elf>",
        n_images,
        firewall::mem_map::TABLE.0
    );
    for item in &report {
        match item {
            ReportItem::Xdp(name) => info!("report xdp {name}"),
            ReportItem::Map(ty, name) => info!("report map {ty:?} {name}"),
        }
    }
    if n_images > 0 {
        match rt.switch(0, 0, Some(&maps)) {
            Ok(()) => match rt.lease(Some(&mut maps)) {
                Ok(()) => info!("xdp leased primary image_id=0"),
                Err(e) => error!("xdp lease failed: {e:?}"),
            },
            Err(e) => error!("xdp switch primary failed: {e:?}"),
        }
    }
    drop(rt);
    drop(maps);

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
            let net_config = static_ipv4_config();
            let (stack, runner) = embassy_net::new(interfaces.station, net_config, resources, seed);
            spawner.spawn(net_task(runner).unwrap());
            spawner.spawn(udp_task(stack).unwrap());
            spawner.spawn(ctrl_task(stack).unwrap());
            spawner.spawn(wifi_task(controller).unwrap());
            info!("sta ssid={ssid} stim={STIM_PORT} ctrl={CTRL_PORT}");
        }
        _ => {
            info!("WIFI_SSID/WIFI_PASS unset; UDP disabled");
        }
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

#[embassy_executor::task]
#[allow(clippy::large_stack_frames, reason = "UDP ack socket buffers")]
async fn udp_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        info!("ipv4 {}", v4.address);
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
        let Ok((n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        NET_RX.fetch_add(1, Ordering::Relaxed);
        let dest = ack_endpoint(meta.endpoint.addr, meta.endpoint.port);
        let _ = socket.send_to(b"ACK\n", dest).await;
        let _ = n;
    }
}

#[embassy_executor::task]
#[allow(clippy::large_stack_frames, reason = "MapCrud ctrl socket buffers")]
async fn ctrl_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    let rx_meta = CTRL_RX_META.init([PacketMetadata::EMPTY; 4]);
    let tx_meta = CTRL_TX_META.init([PacketMetadata::EMPTY; 4]);
    let rx_buf = CTRL_RX_BUF.init([0; 1536]);
    let tx_buf = CTRL_TX_BUF.init([0; 1536]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(CTRL_PORT).expect("bind ctrl");
    info!("map crud listening {CTRL_PORT}");
    let mut buf = [0u8; 512];
    let mut ack_buf = [0u8; 256];
    let mut val_buf = [0u8; 32];
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
        let dest = (meta.endpoint.addr, REPLY_PORT);
        let _ = socket.send_to(&ack_buf[..alen], dest).await;
    }
}

fn handle_map_crud<'a>(pkt: &[u8], val_buf: &'a mut [u8; 32]) -> (u32, u32, &'a [u8]) {
    let Some(op) = map_crud::decode_op(pkt) else {
        return (STATUS_BAD, 0, &[]);
    };
    let kind = op.kind;
    if op.map_name != MAP_BLACKLIST {
        return (STATUS_NOT_FOUND, kind, &[]);
    }
    let Some(lock) = xpost::map::MAP_RT.try_get() else {
        return (STATUS_BAD, kind, &[]);
    };
    let Ok(mut maps) = lock.try_lock() else {
        return (STATUS_BAD, kind, &[]);
    };
    let Ok(idx) = maps.find(MAP_BLACKLIST) else {
        return (STATUS_NOT_FOUND, kind, &[]);
    };
    match op.kind {
        map_crud::KIND_CREATE => (STATUS_CONFLICT, kind, &[]),
        map_crud::KIND_DESTROY_MAP => (STATUS_BAD, kind, &[]),
        map_crud::KIND_LOOKUP => match maps.hash_lookup(idx, op.key, val_buf.as_mut_slice()) {
            Ok(n) => (STATUS_OK, kind, &val_buf[..n.min(val_buf.len())]),
            Err(MapUseError::NotFound) => (STATUS_NOT_FOUND, kind, &[]),
            Err(_) => (STATUS_BAD, kind, &[]),
        },
        map_crud::KIND_UPDATE => {
            let flags = if op.flags == 0 { BPF_ANY } else { op.flags };
            let value = if op.value.is_empty() { &[1u8][..] } else { op.value };
            match maps.hash_update(idx, op.key, value, flags) {
                Ok(()) => (STATUS_OK, kind, &[]),
                Err(MapUseError::Exists) => (STATUS_CONFLICT, kind, &[]),
                Err(MapUseError::NotFound) => (STATUS_NOT_FOUND, kind, &[]),
                Err(MapUseError::NoMem) => (STATUS_BAD, kind, &[]),
                Err(_) => (STATUS_BAD, kind, &[]),
            }
        }
        map_crud::KIND_DELETE => match maps.hash_delete(idx, op.key) {
            Ok(()) => (STATUS_OK, kind, &[]),
            Err(MapUseError::NotFound) => (STATUS_NOT_FOUND, kind, &[]),
            Err(_) => (STATUS_BAD, kind, &[]),
        },
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
        esp_println::println!("OK perf clear");
    } else if line.eq_ignore_ascii_case("blacklist clear") {
        match blacklist_clear() {
            Ok(()) => esp_println::println!("OK blacklist clear"),
            Err(e) => esp_println::println!("ERR blacklist clear {e}"),
        }
    } else if let Some(rest) = strip_cmd(line, "map dump") {
        print_map_dump(rest);
    } else if line.eq_ignore_ascii_case("map dump") {
        print_map_dump("");
    } else if let Some(rest) = strip_cmd(line, "ack-dest") {
        match parse_ack_dest(rest) {
            Some((ip, port)) => {
                ACK_IP.store(ip, Ordering::Relaxed);
                ACK_PORT.store(u32::from(port), Ordering::Relaxed);
                esp_println::println!("OK ack-dest {rest}");
            }
            None => esp_println::println!("ERR ack-dest <ipv4> <port>"),
        }
    } else if line.eq_ignore_ascii_case("heap") {
        esp_println::println!(
            "OK heap used={} free={}",
            esp_alloc::HEAP.used(),
            esp_alloc::HEAP.free()
        );
    } else {
        esp_println::println!("ERR unknown");
    }
}

fn blacklist_clear() -> Result<(), &'static str> {
    let lock = xpost::map::MAP_RT.try_get().ok_or("no-rt")?;
    let mut maps = lock.try_lock().map_err(|_| "busy")?;
    let idx = maps.find(MAP_BLACKLIST).map_err(|_| "not-found")?;
    maps.hash_clear(idx).map_err(|_| "clear")?;
    Ok(())
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
    let Some(lock) = xpost::map::MAP_RT.try_get() else {
        esp_println::println!("ERR map no-rt");
        return;
    };
    let Ok(guard) = lock.try_lock() else {
        esp_println::println!("ERR busy");
        return;
    };
    let Ok(id) = guard.find(name) else {
        esp_println::println!("ERR map not-found");
        return;
    };
    let Ok(mg) = guard.get(id) else {
        esp_println::println!("ERR map get");
        return;
    };
    match mg.map_type() {
        xpost::map::MapType::Array => {
            let n = mg.len();
            esp_println::println!("OK map {name} type=array size={n}");
            for i in 0..n {
                match mg.array_get_u64(i as u32) {
                    Ok(v) => esp_println::println!("{i} {v}"),
                    Err(_) => {
                        esp_println::println!("ERR map slot={i}");
                        return;
                    }
                }
            }
            esp_println::println!("OK map end");
        }
        xpost::map::MapType::Hash => {
            let n = guard.hash_len(id).unwrap_or(0);
            esp_println::println!("OK map {name} type=hash size={n} cap={}", mg.len());
            esp_println::println!("OK map end");
        }
    }
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

fn static_ipv4_config() -> embassy_net::Config {
    let ip = option_env!("STA_IP").unwrap_or("192.168.110.106");
    let gw = option_env!("STA_GW").unwrap_or("192.168.110.1");
    let prefix: u8 = option_env!("STA_PREFIX")
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let address = parse_v4(ip).unwrap_or_else(|| panic!("STA_IP invalid: {ip}"));
    let gateway = parse_v4(gw).unwrap_or_else(|| panic!("STA_GW invalid: {gw}"));
    let addr = Ipv4Address::new(
        (address >> 24) as u8,
        (address >> 16) as u8,
        (address >> 8) as u8,
        address as u8,
    );
    let gw_addr = Ipv4Address::new(
        (gateway >> 24) as u8,
        (gateway >> 16) as u8,
        (gateway >> 8) as u8,
        gateway as u8,
    );
    embassy_net::Config::ipv4_static(StaticConfigV4 {
        address: Ipv4Cidr::new(addr, prefix),
        gateway: Some(gw_addr),
        dns_servers: Default::default(),
    })
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
