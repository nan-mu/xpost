#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

use core::convert::Infallible;
use core::ptr::{addr_of, addr_of_mut};

use alloc::string::ToString;
use portable_atomic::{AtomicUsize, Ordering};

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
use xpost::map::dump::{array_blob_len, write_array_blob};
use xpost::map::{BPF_ANY, HEAP_SIZE, MapRt, MapUseError};
use xpost::xdp::{ReportItem, Slot, XdpRt};
#[allow(unused_imports)] // PerfError only when neither perf-cag nor perf-t
use xpost_serial::reply::{MapError, OkReply, PerfError, ServerError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

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
/// Virtual map name for legal-flow u16 tokens (`get/clear map firewall_result`).
const MAP_FIREWALL_RESULT: &str = "firewall_result";
const MAP_DROPPED: &str = "firewall_dropped";
const LEGAL_CAP: usize = 16384;

static LEGAL_LEN: AtomicUsize = AtomicUsize::new(0);
static LEGAL_DROPPED: AtomicUsize = AtomicUsize::new(0);
#[unsafe(link_section = ".bss")]
static mut LEGAL_TOKENS: [u16; LEGAL_CAP] = [0; LEGAL_CAP];

#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Primary RX slot. Loaded before lease; immutable while it is the entry.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

/// Idle slot. Not the datapath entry until a leased `switch` retargets it.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_1")]
static xdp_slot_idle: Slot<0xE00> = Slot::new([0u8; 0xE00]);

static NET_RX: AtomicUsize = AtomicUsize::new(0);

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

    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap).await.expect("MapRt::init");
    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }, unsafe {
        xdp_slot_idle.as_xdp_slot(false)
    }];
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

    spawner.spawn(control_serial(usb_rx).unwrap());
    esp_println::println!(
        "xpost ready ipv4={} port={STIM_PORT}",
        option_env!("STA_IP").unwrap_or("?")
    );

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
        let Ok((n, _meta)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        NET_RX.fetch_add(1, Ordering::Relaxed);
        // Legal (XDP PASS) flow: record LE u16 token; never ACK.
        if n >= 2 {
            let token = u16::from_le_bytes([buf[0], buf[1]]);
            note_legal(token);
        }
    }
}

fn note_legal(token: u16) {
    critical_section::with(|_| {
        let len = LEGAL_LEN.load(Ordering::Relaxed);
        if len >= LEGAL_CAP {
            LEGAL_DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        unsafe {
            (*addr_of_mut!(LEGAL_TOKENS))[len] = token;
        }
        LEGAL_LEN.store(len + 1, Ordering::Relaxed);
    });
}

fn clear_legal_tokens() {
    LEGAL_LEN.store(0, Ordering::Relaxed);
    LEGAL_DROPPED.store(0, Ordering::Relaxed);
}

fn reply_dropped(reply: ReplyWriter<'_>) {
    let n = LEGAL_DROPPED.load(Ordering::Relaxed) as u64;
    let len = array_blob_len(1, |_| n);
    reply.get_map(len, |write| {
        write_array_blob(1, |_| n, &mut |chunk| {
            let _ = write(chunk);
        });
    });
}

fn legal_token(i: u32) -> u64 {
    u64::from(unsafe { (*addr_of!(LEGAL_TOKENS))[i as usize] })
}

fn reply_legal_tokens(reply: ReplyWriter<'_>) {
    critical_section::with(|_| {
        let size = LEGAL_LEN.load(Ordering::Relaxed) as u32;
        let len = array_blob_len(size, legal_token);
        reply.get_map(len, |write| {
            write_array_blob(size, legal_token, &mut |chunk| {
                let _ = write(chunk);
            });
        });
    });
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
            let value = if op.value.is_empty() {
                &[1u8][..]
            } else {
                op.value
            };
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
        .on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
            if name == MAP_FIREWALL_RESULT {
                clear_legal_tokens();
                reply.ok(OkReply::ClearMap);
                return;
            }
            match app_ctrl::clear_map(name) {
                Ok(()) => reply.ok(OkReply::ClearMap),
                Err(e) => reply_map_err(reply, e),
            }
        })
        .on_get_map(|name: &str, reply: ReplyWriter<'_>| {
            if name == MAP_FIREWALL_RESULT {
                reply_legal_tokens(reply);
                return;
            }
            if name == MAP_DROPPED {
                reply_dropped(reply);
                return;
            }
            match app_ctrl::lock_map(name) {
                Ok((maps, idx)) => match maps.map_blob_len(idx) {
                    Ok(len) => reply.get_map(len, |write| {
                        let _ = maps.write_map_blob(idx, |chunk| {
                            let _ = write(chunk);
                        });
                    }),
                    Err(e) => reply_map_err(reply, e.into()),
                },
                Err(e) => reply_map_err(reply, e),
            }
        })
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                if name == MAP_FIREWALL_RESULT {
                    reply.error(&MapError::Unsupported);
                    return;
                }
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
        .build()
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
