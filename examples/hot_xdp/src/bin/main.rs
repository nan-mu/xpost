#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::convert::Infallible;
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
use xpost::app_ctrl::{self, AppMapError, AppXdpError};
use xpost::map::{HEAP_SIZE, MapRt};
use xpost::xdp::{ReportItem, Slot, XdpRt};
#[allow(unused_imports)] // PerfError only when neither perf-cag nor perf-t
use xpost_serial::reply::{MapError, OkReply, PerfError, ServerError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// UDP port the netstack accepts. Drop XDP never reaches this socket.
const STIM_PORT: u16 = 8080;

/// Map talc heap — size must match `[package.metadata.xpost.map].heap_size`.
#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Primary RX slot. Loaded before lease; immutable while it is the entry.
/// Symbol name matches xpost-build `SLOT_SYMBOL`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

/// Idle slot. Staged while slot `a` is the entry; published only by `xdp activate`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_1")]
static xdp_slot_idle: Slot<0xE00> = Slot::new([0u8; 0xE00]);

/// Packets that reached the netstack (XDP Pass). Drop XDP does not increment this.
static NET_RX: AtomicUsize = AtomicUsize::new(0);
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
    // Quiet USB: info/trace during XDP_TX stress contends with the agent session
    // and shows up as host send gaps (formal: 80–145 ms) that poison loss medians.
    esp_println::logger::init_logger(log::LevelFilter::Error);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let flash = peripherals.FLASH;

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);
    // Wi-Fi dynamic RX/TX buffers come from this heap; must match examples/just_tx_hal.
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    info!("Embassy initialized!");

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();

    // `main -> !` cannot return `Result`; boot-fatal errors panic into our handler.
    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap).await.expect("MapRt::init");

    xpost::xdp::set_boot_id(boot_entropy());
    let slots = [
        unsafe { xdp_slot.as_xdp_slot(true) },
        unsafe { xdp_slot_idle.as_xdp_slot(false) },
    ];

    let (mut rt, report) = XdpRt::init(
        flash,
        &slots,
        hot_xdp::mem_map::TABLE,
        &mut maps,
        hot_xdp::mem_map::HELPER_DISPATCHER_BASE,
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
        hot_xdp::mem_map::TABLE.0
    );
    for item in &report {
        match item {
            ReportItem::Xdp(name) => info!("report xdp {name}"),
            ReportItem::Map(ty, name) => info!("report map {ty:?} {name}"),
        }
    }
    // Cold image stays Pass until switched into the primary IRAM slot and leased.
    // Soft-fail: a stale cold ELF (wrong link_addr after BSS shift) must not panic
    // the board — leave Prog on Pass until `agent program` writes a matching image.
    let slot0 = &xdp_slot as *const Slot<0xE00> as usize;
    if let Some(id) = rt.image_linked_at(slot0) {
        match rt.switch(id, 0, Some(&maps)) {
            Ok(()) => match rt.lease(Some(&mut maps)) {
                Ok(()) => info!("xdp leased image_id={id} linked at slot 0"),
                Err(e) => error!("xdp lease failed: {e:?}"),
            },
            Err(e) => error!("xdp switch primary failed: {e:?} (cold image link_addr mismatch?)"),
        }
    } else if n_images > 0 {
        error!("no image linked at slot 0; Prog stays Pass");
    } else {
        info!("xdp cold empty; Prog Pass until program+reboot");
    }
    // Release MapRtGuard: RX helpers use try_lock; holding this forever → Busy / helper_map++.
    drop(rt);
    drop(maps);
    info!("map_rt unlocked (RX helpers can proceed)");

    // App serial after MapRt/XdpRt init so `OK init` means control plane is ready.
    spawner.spawn(control_serial(usb_rx).unwrap());
    esp_println::println!(
        "xpost ready ipv4={} port={STIM_PORT}",
        option_env!("STA_IP").unwrap_or("?")
    );

    // Must match examples/just_tx_hal (A/B fairness): dynamic TX/RX at ESP-IDF max.
    // AMPDU TX off: its BA session stalls TX ~1.07 s under ping-pong load.
    let wifi_cfg = esp_radio::wifi::ControllerConfig::default()
        .with_dynamic_tx_buf_num(128)
        .with_dynamic_rx_buf_num(1024)
        .with_ampdu_tx_enable(false);
    let (mut controller, interfaces) = xpost::esp_radio::new(peripherals.WIFI, wifi_cfg)
        .expect("Failed to initialize Wi-Fi controller");
    info!(
        "wifi+xdp attached drop={} failed_tx={} tx_dyn=128 rx_dyn=1024",
        xpost::ext::perf::counter().drop_count(),
        xpost::ext::perf::counter().failed_tx_count(),
    );

    // XDP only enqueues from the Wi-Fi RX callback; this task owns tx_by_ref.
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
            // Always arm a short watchdog: missed `tx_done` kicks (or NO_MEM with
            // a stuck inflight count) must not park the drain task indefinitely —
            // that is what turns brief air stalls into 0.5–1 s RTT tails.
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
        info!("ipv4 {}", v4.address);
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
        NET_RX.fetch_add(1, Ordering::Relaxed);
        let dest = ack_endpoint(meta.endpoint.addr, meta.endpoint.port);
        let payload = b"ACK\n";
        if socket.send_to(payload, dest).await.is_err() {
            error!("ack send failed bytes={n}");
        }
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
        .on_enable_xdp(|reply: ReplyWriter<'_>| match app_ctrl::enable_xdp() {
            Ok(()) => reply.ok(OkReply::EnableXdp),
            Err(e) => reply_xdp_err(reply, e),
        })
        .on_disable_xdp(|reply: ReplyWriter<'_>| match app_ctrl::disable_xdp() {
            Ok(()) => reply.ok(OkReply::DisableXdp),
            Err(e) => reply_xdp_err(reply, e),
        })
        .on_clear_map(|name: &str, reply: ReplyWriter<'_>| match app_ctrl::clear_map(name) {
            Ok(()) => reply.ok(OkReply::ClearMap),
            Err(e) => reply_map_err(reply, e),
        })
        .on_get_map(|name: &str, reply: ReplyWriter<'_>| match app_ctrl::lock_map(name) {
            Ok((maps, idx)) => match maps.map_blob_len(idx) {
                Ok(len) => reply.get_map(len, |write| {
                    let _ = maps.write_map_blob(idx, |chunk| {
                        let _ = write(chunk);
                    });
                }),
                Err(e) => reply_map_err(reply, e.into()),
            },
            Err(e) => reply_map_err(reply, e),
        })
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                match app_ctrl::update_map(name, key, value) {
                    Ok(()) => reply.ok(OkReply::UpdateMap),
                    Err(e) => reply_map_err(reply, e),
                }
            },
        )
        .on_clear_perf(|reply: ReplyWriter<'_>| {
            #[cfg(any(feature = "perf-cag", feature = "perf-t"))]
            {
                xpost::ext::perf::clear_perf();
                reply.ok(OkReply::ClearPerf);
            }
            #[cfg(not(any(feature = "perf-cag", feature = "perf-t")))]
            {
                reply.error(&PerfError::Unavailable);
            }
        })
        .on_get_perf(|reply: ReplyWriter<'_>| {
            #[cfg(any(feature = "perf-cag", feature = "perf-t"))]
            {
                let n = xpost::ext::perf::report_payload_len();
                reply.get_perf(n, |write| {
                    xpost::ext::perf::report_perf(write);
                });
            }
            #[cfg(not(any(feature = "perf-cag", feature = "perf-t")))]
            {
                reply.error(&PerfError::Unavailable);
            }
        })
        .on_xdp_stage(|name: &str, slot: u8, reply: ReplyWriter<'_>| {
            match app_ctrl::stage_xdp(name, slot) {
                Ok(generation) => reply.ok(OkReply::XdpStage { slot, generation }),
                Err(e) => reply_xdp_err(reply, e),
            }
        })
        .on_xdp_activate(|slot: u8, generation: u32, pass: bool, reply: ReplyWriter<'_>| {
            match app_ctrl::activate_xdp(slot, generation, pass) {
                Ok(report) => reply.ok(activate_reply(&report)),
                Err(e) => reply_xdp_err(reply, e),
            }
        })
        .on_xdp_rollback(|pass: bool, reply: ReplyWriter<'_>| {
            match app_ctrl::rollback_xdp(pass) {
                Ok(report) => reply.ok(rollback_reply(&report)),
                Err(e) => reply_xdp_err(reply, e),
            }
        })
        .on_get_xdp_status(|reply: ReplyWriter<'_>| match app_ctrl::xdp_status() {
            Ok(status) => reply.ok(status_reply(&status)),
            Err(e) => reply_xdp_err(reply, e),
        })
        .build()
    }
}

fn boot_entropy() -> u32 {
    let id = esp_hal::rng::Rng::new().random();
    if id == 0 { 1 } else { id }
}

fn slot_byte(slot: Option<usize>) -> u8 {
    slot.and_then(|s| u8::try_from(s).ok()).unwrap_or(255)
}

fn activate_reply(report: &xpost::xdp::SwitchReport) -> OkReply<'static> {
    OkReply::XdpActivate {
        ok: u8::from(report.ok),
        rollback: u8::from(report.rolled_back),
        severe: u8::from(report.severe),
        from_slot: report.from_slot as u8,
        to_slot: report.to_slot as u8,
        generation: report.generation,
        quiesce_drop: report.quiesce_drop,
        quiesce_pass: report.quiesce_pass,
    }
}

fn rollback_reply(report: &xpost::xdp::SwitchReport) -> OkReply<'static> {
    OkReply::XdpRollback {
        ok: u8::from(report.ok),
        rollback: u8::from(report.rolled_back),
        severe: u8::from(report.severe),
        from_slot: report.from_slot as u8,
        to_slot: report.to_slot as u8,
        generation: report.generation,
        quiesce_drop: report.quiesce_drop,
        quiesce_pass: report.quiesce_pass,
    }
}

fn status_reply(status: &xpost::xdp::XdpStatus) -> OkReply<'static> {
    OkReply::GetXdpStatus {
        boot_id: status.boot_id,
        active_slot: slot_byte(status.active_slot),
        staged_slot: slot_byte(status.staged_slot),
        generation: status.staged_generation,
        retained_slot: slot_byte(status.retained_slot),
        updating: u8::from(status.updating),
        severe: u8::from(status.severe),
        quiesce_drop: status.quiesce_drop,
        quiesce_pass: status.quiesce_pass,
        last_error: status.last_error,
    }
}

fn reply_map_err(reply: ReplyWriter<'_>, e: AppMapError) {
    match e {
        AppMapError::Busy => reply.error(&ServerError::Busy),
        AppMapError::NoRt | AppMapError::Logic => reply.error(&ServerError::Logic),
        AppMapError::NameNotFound => reply.error(&MapError::NameNotFound),
        AppMapError::InvalidName => reply.error(&MapError::InvalidName),
        AppMapError::InvalidKey => reply.error(&MapError::InvalidKey),
        AppMapError::InvalidValue => reply.error(&MapError::InvalidValue),
        AppMapError::Full => reply.error(&MapError::Full),
        AppMapError::Unsupported => reply.error(&MapError::Unsupported),
    }
}

fn reply_xdp_err(reply: ReplyWriter<'_>, e: AppXdpError) {
    match e {
        AppXdpError::Busy => reply.error(&ServerError::Busy),
        AppXdpError::NoRt | AppXdpError::Logic => reply.error(&ServerError::Logic),
    }
}

fn static_ipv4_config() -> embassy_net::Config {
    let ip = option_env!("STA_IP").unwrap_or("192.168.110.106");
    let gw = option_env!("STA_GW").unwrap_or("192.168.110.1");
    let prefix: u8 = option_env!("STA_PREFIX")
        .and_then(|s| s.parse().ok())
        .unwrap_or(24);
    let address = parse_v4(ip).unwrap_or_else(|| {
        panic!("STA_IP invalid: {ip}");
    });
    let gateway = parse_v4(gw).unwrap_or_else(|| {
        panic!("STA_GW invalid: {gw}");
    });
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
    info!("sta static {ip}/{prefix} gw={gw}");
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
