#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use alloc::string::ToString;
use portable_atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

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

/// UDP port the netstack accepts (same stim surface as just_drop).
const STIM_PORT: u16 = 8080;

/// IRAM slot required by xpost-build link layout; HAL never leases a cold image
/// (inactive Prog → Pass). Symbol name matches xpost-build `SLOT_SYMBOL`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

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
    spawner.spawn(serial_task(usb_rx).unwrap());
    // Host opens App UART ~800ms after reset; delay so settle can latch this.
    Timer::after(Duration::from_millis(1200)).await;
    esp_println::println!("xpost ready ipv4=? port={STIM_PORT}");

    // Keep the IRAM slot symbol live for the linker; never lease.
    let _slot = core::ptr::addr_of!(xdp_slot);

    // Path esp-radio + xpost: inactive Prog → Pass; app still drops at UDP.
    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false);
    let (mut controller, interfaces) = xpost::esp_radio::new(peripherals.WIFI, wifi_cfg)
        .expect("Failed to initialize Wi-Fi controller");
    info!("wifi+xdp Pass (no lease) drop_site=app tx_dyn=128 rx_dyn=1024");

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
            info!("WIFI_SSID/WIFI_PASS unset; UDP ack disabled, serial counters still live");
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
    } else if line.eq_ignore_ascii_case("counters") || line.eq_ignore_ascii_case("report counters")
    {
        print_counters();
    } else if line.eq_ignore_ascii_case("agg") {
        print_agg();
    } else if line.eq_ignore_ascii_case("gauge") {
        print_gauge();
    } else if line.eq_ignore_ascii_case("trace") {
        print_trace_summary();
    } else if line.eq_ignore_ascii_case("trace clear") {
        #[cfg(feature = "kind-t")]
        xpost::ext::perf::trace().clear();
        esp_println::println!("OK trace clear");
    } else if line.eq_ignore_ascii_case("perf clear") {
        perf_clear();
        esp_println::println!("OK perf clear");
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
    } else if line.eq_ignore_ascii_case("drop on") {
        DROP_ARMED.store(true, Ordering::Relaxed);
        esp_println::println!("OK drop on");
    } else if line.eq_ignore_ascii_case("drop off") {
        DROP_ARMED.store(false, Ordering::Relaxed);
        esp_println::println!("OK drop off");
    } else if line.eq_ignore_ascii_case("counters") {
        let drop = DROP.load(Ordering::Relaxed);
        let net_rx = NET_RX.load(Ordering::Relaxed);
        esp_println::println!("OK counters drop={drop} net_rx={net_rx}");
    } else {
        esp_println::println!("ERR unknown");
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "single long println! of many counter fields"
)]
fn print_counters() {
    let t = xpost::ext::perf::counter();
    // `drop=` is app-layer DROP (formal primary); EspRadioTrace fields follow just_drop.
    esp_println::println!(
        "OK counters drop={} failed_tx={} tx_bad_buf={} tx_token={} tx_wifi={} tx_wifi_rc={} net_rx={} inactive_skip={} reentry={} stack_misalign={} bad_md={} helper_unresolved={} helper_map={} pool_drop_misalign={} map_lock={} tx_enq={} tx_pend={} tx_inflight={} tx_drain_ok={} tx_nomem_wait={} tx_cap_wait={} tx_enq_claim={} tx_enq_ring={} tx_done={} tx_done_fail={} tx_grp_src={} tx_ns_ref={} tx_ns_free={} tx_no_retain={} tx_flush={} tx_last_rc={} rx_packets={} xdp_invocations={} xdp_pass={} xdp_drop={} xdp_tx={} xdp_aborted={} rx_delivered={} rx_queue_overflow={} runtime_errors={}",
        DROP.load(Ordering::Relaxed),
        t.failed_tx_count(),
        t.failed_tx_bad_buf_count(),
        t.failed_tx_token_count(),
        t.failed_tx_wifi_count(),
        t.failed_tx_wifi_last_rc(),
        NET_RX.load(Ordering::Relaxed),
        t.inactive_skip(),
        t.reentry(),
        t.stack_misalign(),
        t.bad_md(),
        t.helper_unresolved(),
        t.helper_map(),
        t.pool_drop_misalign(),
        "no-rt",
        t.tx_enq(),
        t.tx_pend(),
        t.tx_inflight(),
        t.tx_drain_ok(),
        t.tx_nomem_wait(),
        t.tx_cap_wait(),
        t.tx_enq_claim(),
        t.tx_enq_ring(),
        t.tx_done(),
        t.tx_done_fail(),
        t.tx_grp_src(),
        t.tx_ns_ref(),
        t.tx_ns_free(),
        t.tx_no_retain(),
        t.tx_flush(),
        t.tx_last_rc(),
        t.rx_packets(),
        t.xdp_invocations(),
        t.xdp_pass(),
        t.xdp_drop(),
        t.xdp_tx(),
        t.xdp_aborted(),
        t.rx_delivered(),
        t.rx_queue_overflow(),
        t.runtime_errors(),
    );
}

fn print_agg() {
    #[cfg(feature = "kind-a")]
    {
        // Single OK line so agent `request` captures every hist (first OK wins).
        let a = xpost::ext::perf::aggregate();
        esp_println::print!("OK agg");
        for (name, snap) in [
            ("rx_processing", a.rx_processing.snapshot()),
            ("xdp_execution", a.xdp_execution.snapshot()),
            ("xdp_dispatch", a.xdp_dispatch.snapshot()),
            ("post_action", a.post_action.snapshot()),
        ] {
            esp_println::print!(
                " | {name} count={} total={} min={} max={} lo={} width={} buckets=",
                snap.count,
                snap.total_ticks,
                snap.min_ticks,
                snap.max_ticks,
                snap.lo,
                snap.width
            );
            for (i, b) in snap.buckets.iter().enumerate() {
                if i > 0 {
                    esp_println::print!(",");
                }
                esp_println::print!("{b}");
            }
        }
        esp_println::println!();
    }
    #[cfg(not(feature = "kind-a"))]
    esp_println::println!("ERR agg feature=kind-a required");
}

fn print_gauge() {
    #[cfg(feature = "kind-g")]
    {
        let g = xpost::ext::perf::gauge();
        esp_println::println!(
            "OK gauge rx_queue_depth={},{} tx_queue_depth={},{} tx_inflight={},{} trace_buffer_usage={},{}",
            g.rx_queue_depth.current(),
            g.rx_queue_depth.peak(),
            g.tx_queue_depth.current(),
            g.tx_queue_depth.peak(),
            g.tx_inflight.current(),
            g.tx_inflight.peak(),
            g.trace_buffer_usage.current(),
            g.trace_buffer_usage.peak(),
        );
    }
    #[cfg(not(feature = "kind-g"))]
    esp_println::println!("ERR gauge feature=kind-g required");
}

fn print_trace_summary() {
    #[cfg(feature = "kind-t")]
    {
        let t = xpost::ext::perf::trace();
        esp_println::println!(
            "OK trace len={} dropped={} cap={}",
            t.len(),
            t.dropped(),
            xpost::ext::perf::TRACE_CAP
        );
    }
    #[cfg(not(feature = "kind-t"))]
    esp_println::println!("ERR trace feature=kind-t required");
}

fn print_trace_dump() {
    #[cfg(feature = "kind-t")]
    {
        let t = xpost::ext::perf::trace();
        let mut buf = [xpost::ext::perf::Event::default(); xpost::ext::perf::TRACE_CAP];
        let n = t.snapshot(&mut buf);
        esp_println::print!(
            "OK tracedump len={} dropped={} cap={}",
            n,
            t.dropped(),
            xpost::ext::perf::TRACE_CAP
        );
        for ev in buf.iter().take(n) {
            let name = match xpost::ext::perf::EventKind::from_u8(ev.kind) {
                Some(xpost::ext::perf::EventKind::RxBegin) => "RxBegin",
                Some(xpost::ext::perf::EventKind::XdpBegin) => "XdpBegin",
                Some(xpost::ext::perf::EventKind::XdpEnd) => "XdpEnd",
                Some(xpost::ext::perf::EventKind::RxEnd) => "RxEnd",
                Some(xpost::ext::perf::EventKind::SlowExecution) => "SlowExecution",
                Some(xpost::ext::perf::EventKind::RxOverflow) => "RxOverflow",
                Some(xpost::ext::perf::EventKind::RuntimeError) => "RuntimeError",
                Some(xpost::ext::perf::EventKind::UserMarker) => "UserMarker",
                None => "Unknown",
            };
            esp_println::print!(
                " | {name} t_us={} arg0={} arg1={}",
                ev.t_us, ev.arg0, ev.arg1
            );
        }
        esp_println::println!();
    }
    #[cfg(not(feature = "kind-t"))]
    esp_println::println!("ERR tracedump feature=kind-t required");
}

fn perf_clear() {
    #[cfg(feature = "kind-c")]
    xpost::ext::perf::counter().reset_all();
    #[cfg(feature = "kind-a")]
    xpost::ext::perf::aggregate().reset_all();
    #[cfg(feature = "kind-g")]
    xpost::ext::perf::gauge().reset_all();
    #[cfg(feature = "kind-t")]
    xpost::ext::perf::trace().clear();
    DROP.store(0, Ordering::Relaxed);
    NET_RX.store(0, Ordering::Relaxed);
}

fn print_map_dump(rest: &str) {
    let name = rest.trim();
    if name.is_empty() {
        esp_println::println!("ERR map dump <name>");
        return;
    }
    if !name.eq_ignore_ascii_case("counter") {
        esp_println::println!("ERR map not-found");
        return;
    }
    const N: usize = 256;
    esp_println::println!("OK map counter type=array size={N}");
    for i in 0..N {
        let v = COUNTER[i].load(Ordering::Relaxed);
        esp_println::println!("{i} {v}");
    }
    esp_println::println!("OK map end");
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

fn ipv4_last_octet(addr: embassy_net::IpAddress) -> Option<u8> {
    let embassy_net::IpAddress::Ipv4(v4) = addr;
    Some(v4.octets()[3])
}
