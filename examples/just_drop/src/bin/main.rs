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
use embassy_net::{Ipv4Address, Ipv4Cidr, StackResources, StaticConfigV4};
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info};
use static_cell::StaticCell;
use xpost::map::{HEAP_SIZE, MapRt};
use xpost::xdp::{ReportItem, Slot, XdpRt};

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

/// Single RX slot (firmware-owned). Symbol name matches xpost-build `SLOT_SYMBOL`.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".bss")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

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
    spawner.spawn(serial_task(usb_rx).unwrap());
    // Host `reset_to_app` sleeps ~800ms before opening the App UART reader;
    // printing earlier loses the settle marker. Delay past that window.
    Timer::after(Duration::from_millis(1200)).await;
    esp_println::println!(
        "xpost ready ipv4={} port={STIM_PORT}",
        option_env!("STA_IP").unwrap_or("?")
    );

    // `main -> !` cannot return `Result`; boot-fatal errors panic into our handler.
    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap).await.expect("MapRt::init");

    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }];

    let (rt, report) = XdpRt::init(flash, &slots, just_drop::mem_map::TABLE, &mut maps)
        .await
        .expect("XdpRt::init");
    info!(
        "xdp init ok images={} write-bin {:#x} <xdp.elf>",
        rt.image_count(),
        just_drop::mem_map::TABLE.0
    );
    for item in &report {
        match item {
            ReportItem::Xdp(name) => info!("report xdp {name}"),
            ReportItem::Map(ty, name) => info!("report map {ty:?} {name}"),
        }
    }

    let (mut controller, interfaces) =
        xpost::esp_radio::new(peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    info!(
        "wifi+xdp attached drop={} failed_tx={}",
        xpost::esp_radio::XDP_PROG.drop_count(),
        xpost::esp_radio::XDP_PROG.failed_tx_count()
    );

    match (option_env!("WIFI_SSID"), option_env!("WIFI_PASS")) {
        (Some(ssid), Some(pass)) if !ssid.is_empty() => {
            let station = esp_radio::wifi::Config::Station(
                esp_radio::wifi::sta::StationConfig::default()
                    .with_ssid(ssid)
                    .with_password(pass.to_string()),
            );
            controller
                .set_config(&station)
                .expect("wifi set_config");
            let seed = 0xC3C3_5A5A;
            let resources = NET_RESOURCES.init(StackResources::new());
            let net_config = static_ipv4_config();
            let (stack, runner) =
                embassy_net::new(interfaces.station, net_config, resources, seed);
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
                select(
                    XDP_TX_KICK.wait(),
                    Timer::after(Duration::from_micros(500)),
                )
                .await;
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
async fn net_task(
    mut runner: embassy_net::Runner<'static, esp_radio::wifi::Interface<'static>>,
) {
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

#[embassy_executor::task]
async fn serial_task(mut rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>) {
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

#[allow(
    clippy::large_stack_frames,
    reason = "dispatch fans out to report helpers with large format temporaries"
)]
fn dispatch_serial(line: &str) {
    let line = line.trim();
    if line.eq_ignore_ascii_case("ping") {
        esp_println::println!("OK ping");
    } else if line.eq_ignore_ascii_case("counters") || line.eq_ignore_ascii_case("report counters")
    {
        print_counters();
    } else if line.eq_ignore_ascii_case("txstats") {
        print_txstats();
    } else if line.eq_ignore_ascii_case("txlog") {
        print_txlog();
    } else if line.eq_ignore_ascii_case("txlogv") {
        print_txlogv();
    } else if line.eq_ignore_ascii_case("tx clear") || line.eq_ignore_ascii_case("txstats clear") {
        esp_radio::wifi::xdp_tx_clear_stats();
        xpost::Tracer.clear_tx();
        esp_println::println!("OK tx clear");
    } else if line.eq_ignore_ascii_case("xdpcyc") {
        let [cs, inv, total, n] = xpost::xdp::ingress_cyc::snapshot();
        let n = n.max(1);
        let t0: u32;
        unsafe { core::arch::asm!("csrr {0}, 0x7e2", out(reg) t0) };
        for _ in 0..100 {
            core::hint::black_box(esp_hal::time::Instant::now());
        }
        let t1: u32;
        unsafe { core::arch::asm!("csrr {0}, 0x7e2", out(reg) t1) };
        esp_println::println!(
            "OK xdpcyc n={n} cs_enter={} invoke={} total={} (cycles/pkt) instant_now={}",
            cs / n,
            inv / n,
            total / n,
            t1.wrapping_sub(t0) / 100
        );
    } else if line.eq_ignore_ascii_case("cal") {
        let (ram, flash) = (cal_loop_ram(), cal_loop_flash());
        let rd = || {
            let v: u32;
            unsafe { core::arch::asm!("csrr {0}, 0x7e2", out(reg) v) };
            v
        };
        let t0 = rd();
        for _ in 0..100 {
            critical_section::with(|_| core::hint::black_box(()));
        }
        let cs = rd().wrapping_sub(t0) / 100;
        let a = core::hint::black_box(AtomicU32::new(0));
        let t0 = rd();
        for _ in 0..100 {
            a.fetch_add(1, Ordering::Relaxed);
        }
        let at = rd().wrapping_sub(t0) / 100;
        esp_println::println!(
            "OK cal ram_1000_iter={ram} flash_1000_iter={flash} cs_with={cs} atomic_add={at}"
        );
    } else if line.eq_ignore_ascii_case("xdpcyc clear") {
        xpost::xdp::ingress_cyc::enable();
        xpost::xdp::ingress_cyc::clear();
        esp_println::println!("OK xdpcyc clear");
    } else if line.eq_ignore_ascii_case("heap") {
        esp_println::println!(
            "OK heap used={} free={}",
            esp_alloc::HEAP.used(),
            esp_alloc::HEAP.free()
        );
    } else if let Some(rest) = strip_cmd(line, "txcap") {
        match rest.trim().parse::<usize>() {
            Ok(n) => {
                esp_radio::wifi::xdp_tx_set_inflight_cap(n);
                esp_println::println!("OK txcap {n}");
            }
            _ => esp_println::println!("ERR txcap <n>"),
        }
    } else if line.eq_ignore_ascii_case("gapmon") {
        let (rg, re, dg, de) = esp_radio::wifi::xdp_tx_gapmon();
        let now = esp_hal::time::Instant::now().duration_since_epoch().as_micros() as u32;
        esp_println::println!(
            "OK gapmon rx_gap_us={rg} rx_gap_ago_ms={} done_gap_us={dg} done_gap_ago_ms={}",
            now.wrapping_sub(re) / 1000,
            now.wrapping_sub(de) / 1000
        );
    } else if let Some(rest) = strip_cmd(line, "gapmon") {
        let on = rest.trim().eq_ignore_ascii_case("on");
        esp_radio::wifi::xdp_tx_set_gapmon(on);
        esp_println::println!("OK gapmon {}", if on { "on" } else { "off" });
    } else if let Some(rest) = strip_cmd(line, "txgrp") {
        let drop = rest.trim().eq_ignore_ascii_case("drop");
        esp_radio::wifi::xdp_tx_set_grp_src_drop(drop);
        esp_println::println!("OK txgrp {}", if drop { "drop" } else { "pass" });
    } else if let Some(rest) = strip_cmd(line, "busy") {
        let mut it = rest.split_whitespace();
        let us = it.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        let cs = it.next().is_some_and(|s| s.eq_ignore_ascii_case("cs"));
        esp_radio::wifi::xdp_tx_set_busy(us, cs);
        esp_println::println!("OK busy {us} {}", if cs { "cs" } else { "nocs" });
    } else if let Some(rest) = strip_cmd(line, "xdpbypass") {
        let on = rest.trim().eq_ignore_ascii_case("on");
        esp_radio::wifi::xdp_tx_set_bypass(on);
        esp_println::println!("OK xdpbypass {}", if on { "on" } else { "off" });
    } else if let Some(rest) = strip_cmd(line, "txmode") {
        let copy = rest.trim().eq_ignore_ascii_case("copy");
        esp_radio::wifi::xdp_tx_set_copy(copy);
        esp_println::println!("OK txmode {}", if copy { "copy" } else { "ref" });
    } else if let Some(rest) = strip_cmd(line, "tsflow") {
        let on = rest.trim().eq_ignore_ascii_case("on");
        esp_radio::wifi::xdp_tx_set_tsflow(on);
        esp_println::println!("OK tsflow {}", if on { "on" } else { "off" });
    } else if line.eq_ignore_ascii_case("prof") || line.eq_ignore_ascii_case("flame") {
        print_flame_spans();
    } else if line.eq_ignore_ascii_case("pchits") {
        print_pc_hits();
    } else if line.eq_ignore_ascii_case("prof on") || line.eq_ignore_ascii_case("prof off") {
        let on = line.eq_ignore_ascii_case("prof on");
        esp_radio::wifi::xdp_prof::set_enabled(on);
        esp_println::println!("OK prof {}", if on { "on" } else { "off" });
    } else if line.eq_ignore_ascii_case("prof clear") {
        esp_radio::wifi::xdp_prof::clear_all();
        esp_radio::wifi::xdp_tx_clear_stats();
        xpost::Tracer.clear_tx();
        esp_println::println!("OK prof clear");
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

#[esp_hal::ram]
#[inline(never)]
fn cal_loop_ram() -> u32 {
    cal_body()
}

#[inline(never)]
fn cal_loop_flash() -> u32 {
    cal_body()
}

#[inline(always)]
fn cal_body() -> u32 {
    let t0: u32;
    let t1: u32;
    unsafe {
        core::arch::asm!("csrr {0}, 0x7e2", out(reg) t0);
        core::arch::asm!(
            "li {n}, 1000",
            "2:",
            "addi {n}, {n}, -1",
            "bnez {n}, 2b",
            n = out(reg) _,
        );
        core::arch::asm!("csrr {0}, 0x7e2", out(reg) t1);
    }
    t1.wrapping_sub(t0)
}

fn map_lock_status() -> &'static str {
    match xpost::map::MAP_RT.try_get() {
        None => "no-rt",
        Some(lock) => match lock.try_lock() {
            Ok(_g) => "free",
            Err(_) => "held",
        },
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "single long println! of many counter fields"
)]
fn print_counters() {
    let t = &xpost::Tracer;
    esp_println::println!(
        "OK counters drop={} failed_tx={} tx_bad_buf={} tx_token={} tx_wifi={} tx_wifi_rc={} net_rx={} inactive_skip={} reentry={} stack_misalign={} bad_md={} helper_unresolved={} helper_map={} pool_drop_misalign={} map_lock={} tx_enq={} tx_pend={} tx_inflight={} tx_drain_ok={} tx_nomem_wait={} tx_cap_wait={} tx_enq_claim={} tx_enq_ring={} tx_done={} tx_done_fail={} tx_grp_src={} tx_ns_ref={} tx_ns_free={} tx_no_retain={} tx_flush={} tx_last_rc={}",
        t.drop_count(),
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
        map_lock_status(),
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
    );
}

fn print_txstats() {
    let t = &xpost::Tracer;
    esp_println::println!(
        "OK txstats enq={} enq_claim={} enq_ring={} drain_ok={} nomem_wait={} cap_wait={} drain_fail={} last_rc={} polls={} kicks={} tx_done={} pend={} pend_peak={} inflight={} inflight_peak={} cap={} flow_samples={} done_fail={} block_max_us={} block_gt10ms={} block_gt100ms={} grp_src={} ns_ref={} ns_free={} no_retain={} flush={}",
        t.tx_enq(),
        t.tx_enq_claim(),
        t.tx_enq_ring(),
        t.tx_drain_ok(),
        t.tx_nomem_wait(),
        t.tx_cap_wait(),
        t.tx_submit_fail(),
        t.tx_last_rc(),
        t.tx_polls(),
        t.tx_kicks(),
        t.tx_done(),
        t.tx_pend(),
        t.tx_pending_peak(),
        t.tx_inflight(),
        t.tx_inflight_peak(),
        t.tx_inflight_cap(),
        t.tx_flow_samples(),
        t.tx_done_fail(),
        t.tx_block_max_us(),
        t.tx_block_gt10ms(),
        t.tx_block_gt100ms(),
        t.tx_grp_src(),
        t.tx_ns_ref(),
        t.tx_ns_free(),
        t.tx_no_retain(),
        t.tx_flush(),
    );
}

fn print_txlog() {
    let mut buf = [esp_radio::wifi::XdpTxLogEntry::default(); 32];
    let n = esp_radio::wifi::xdp_tx_log(&mut buf);
    // Single OK line so agent `request` captures the ring (first OK wins).
    esp_println::print!("OK txlog n={n} dropped={}", xpost::Tracer.tx_log_dropped());
    for e in buf.iter().take(n) {
        let kind = esp_radio::wifi::XdpTxEvKind::from_u8(e.kind)
            .map(|k| k.as_str())
            .unwrap_or("?");
        esp_println::print!(
            " | us={} kind={} pend={} infl={} rc={}",
            e.t_us,
            kind,
            e.pending,
            e.inflight,
            e.rc
        );
    }
    esp_println::println!();
}

fn print_txlogv() {
    let mut buf = [esp_radio::wifi::XdpTxLogEntry::default(); 32];
    let n = esp_radio::wifi::xdp_tx_log(&mut buf);
    esp_println::println!("OK txlogv begin n={n} dropped={}", xpost::Tracer.tx_log_dropped());
    for e in buf.iter().take(n) {
        let kind = esp_radio::wifi::XdpTxEvKind::from_u8(e.kind)
            .map(|k| k.as_str())
            .unwrap_or("?");
        esp_println::println!(
            "us={} kind={} pend={} infl={} rc={}",
            e.t_us,
            kind,
            e.pending,
            e.inflight,
            e.rc
        );
    }
    esp_println::println!("OK txlogv end");
}

/// Brendan Gregg folded stacks — paste into speedscope / flamegraph.pl.
fn print_flame_spans() {
    // Single OK line so agent `request` captures every span.
    esp_println::print!("OK flame");
    for s in esp_radio::wifi::xdp_prof::snapshot_spans() {
        if s.cycles == 0 && s.entries == 0 {
            continue;
        }
        esp_println::print!(" | {} {} n={}", s.folded, s.cycles, s.entries);
    }
    esp_println::println!();
}

#[allow(
    clippy::large_stack_frames,
    reason = "PC hit snapshot buffer lives on the stack for one-shot serial dump"
)]
fn print_pc_hits() {
    let mut buf = [esp_radio::wifi::xdp_prof::PcHit { pc: 0, hits: 0 }; 256];
    let n = esp_radio::wifi::xdp_prof::snapshot_pc_hits(&mut buf);
    esp_println::println!(
        "OK pchits n={} samples={} overflow={}",
        n,
        esp_radio::wifi::xdp_prof::pc_samples(),
        esp_radio::wifi::xdp_prof::pc_overflow(),
    );
    for h in buf.iter().take(n) {
        esp_println::println!("pc=0x{:08x} hits={}", h.pc, h.hits);
    }
}

fn print_rxcnt(rest: &str) {
    let Ok(key) = rest.trim().parse::<u32>() else {
        esp_println::println!("ERR rxcnt <key>");
        return;
    };
    let Some(lock) = xpost::map::MAP_RT.try_get() else {
        esp_println::println!("ERR rxcnt no-rt");
        return;
    };
    let Ok(guard) = lock.try_lock() else {
        esp_println::println!("ERR busy");
        return;
    };
    let Ok(id) = guard.find("rxcnt") else {
        esp_println::println!("ERR rxcnt not-found");
        return;
    };
    let Ok(mg) = guard.get(id) else {
        esp_println::println!("ERR rxcnt get");
        return;
    };
    match mg.array_get_u64(key) {
        Ok(v) => esp_println::println!("OK rxcnt key={key} value={v}"),
        Err(_) => esp_println::println!("ERR rxcnt key={key}"),
    }
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
