#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::string::ToString;
use portable_atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info};
use static_cell::StaticCell;
use xpost::xdp::Slot;

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

/// IRAM slot required by xpost-build link layout; HAL never leases a cold image
/// (inactive Prog → Pass). Symbol name matches xpost-build `SLOT_SYMBOL`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

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

    info!("Embassy initialized!");

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    spawner.spawn(serial_task(usb_rx).unwrap());
    // Host opens App UART ~800ms after reset; delay so settle can latch this.
    Timer::after(Duration::from_millis(1200)).await;
    esp_println::println!("xpost ready ipv4=? port={STIM_PORT}");

    // Keep the IRAM slot symbol live for the linker; never lease.
    let _slot = core::ptr::addr_of!(xdp_slot);

    // Path esp-radio + xpost: inactive Prog → Pass; app echoes at UDP.
    // AMPDU TX off: its BA session stalls TX ~1.07 s under ping-pong load.
    // Embassy frame queues (HAL-only; XDP_TX bypasses them): defaults 5/3 drop under burst.
    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false)
        .with_rx_queue_size(32)
        .with_tx_queue_size(16);
    let (mut controller, interfaces) = xpost::esp_radio::new(peripherals.WIFI, wifi_cfg)
        .expect("Failed to initialize Wi-Fi controller");
    info!("wifi+xdp Pass (no lease) echo_site=app tx_dyn=128 rx_dyn=1024 rx_q=32 tx_q=16");

    // Drain path for any accidental XDP_TX; Pass-only should stay Idle.
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
            spawner.spawn(wifi_task(controller).unwrap());
            info!("sta ssid={ssid} stim_port={STIM_PORT}");
        }
        _ => {
            info!("WIFI_SSID/WIFI_PASS unset; UDP echo disabled, serial counters still live");
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
    reason = "embassy UdpSocket future and larger TX/RX scratch buffers"
)]
async fn udp_task(stack: embassy_net::Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(v4) = stack.config_v4() {
        info!("dhcp {}", v4.address);
        esp_println::println!("xpost ready ipv4={} port={STIM_PORT}", v4.address);
    }

    let rx_meta = UDP_RX_META.init([PacketMetadata::EMPTY; 16]);
    let tx_meta = UDP_TX_META.init([PacketMetadata::EMPTY; 16]);
    let rx_buf = UDP_RX_BUF.init([0; 4096]);
    let tx_buf = UDP_TX_BUF.init([0; 4096]);
    let mut socket = UdpSocket::new(stack, rx_meta, rx_buf, tx_meta, tx_buf);
    socket.bind(STIM_PORT).expect("bind stim");
    info!("udp echo listening {STIM_PORT}");

    let mut buf = [0u8; 256];
    loop {
        let Ok((n, meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        NET_RX.fetch_add(1, Ordering::Relaxed);
        RXCNT[IPPROTO_UDP as usize].fetch_add(1, Ordering::Relaxed);
        let dest = ack_endpoint(meta.endpoint.addr, meta.endpoint.port);
        // Await TX fully: short with_timeout was silently dropping every echo
        // (Wi-Fi TX often ≫ 50 ms), so TC never saw bounce and RTT stayed empty.
        // No App* Trace stamps — HAL/XDP flames are not role-comparable under async.
        let send_ok = socket.send_to(&buf[..n], dest).await.is_ok();
        if !send_ok {
            error!("echo send failed bytes={n}");
        }
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
        NET_RX.store(0, Ordering::Relaxed);
        esp_println::println!("OK perf clear");
    } else if line.eq_ignore_ascii_case("prof on") || line.eq_ignore_ascii_case("prof off") {
        let on = line.eq_ignore_ascii_case("prof on");
        xpost::ext::perf::set_prof_enabled(on);
        esp_println::println!("OK prof {}", if on { "on" } else { "off" });
    } else if let Some(rest) = strip_cmd(line, "rxcnt") {
        print_rxcnt(rest);
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

fn print_rxcnt(rest: &str) {
    let Ok(key) = rest.trim().parse::<u32>() else {
        esp_println::println!("ERR rxcnt <key>");
        return;
    };
    if key > 255 {
        esp_println::println!("ERR rxcnt key={key}");
        return;
    }
    let v = RXCNT[key as usize].load(Ordering::Relaxed);
    esp_println::println!("OK rxcnt key={key} value={v}");
}

fn strip_cmd<'a>(line: &'a str, cmd: &str) -> Option<&'a str> {
    let rest = line.get(cmd.len()..)?;
    if !line[..cmd.len()].eq_ignore_ascii_case(cmd) {
        return None;
    }
    let rest = rest.trim_start();
    if rest.is_empty() { None } else { Some(rest) }
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
