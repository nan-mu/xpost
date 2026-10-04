#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
)]
#![deny(clippy::large_stack_frames)]

use core::convert::Infallible;
use core::ptr::addr_of_mut;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::{error, info};
use xpost::app_ctrl::{self, AppMapError, AppXdpError};
use xpost::map::{BPF_ANY, HEAP_SIZE, MapRt};
use xpost::xdp::{ReportItem, Slot, XdpRt};
use xpost_serial::reply::{MapError, OkReply, ServerError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

use exec_bench_common::BUF_CAP;

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Compact ELF V0 map value size for `workload` (see `ebpf/bpf.toml`).
const WORKLOAD_VALUE_LEN: usize = 32;
const MAP_NAME: &str = "workload";
const KEY_INPUT: u8 = 0;
const KEY_OUTPUT: u8 = 1;

#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

static mut BENCH_BUF: [u8; BUF_CAP] = [0u8; BUF_CAP];

#[allow(
    clippy::large_stack_frames,
    reason = "embassy main allocates larger boot buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Error);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let flash = peripherals.FLASH;

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();

    // Lease before App serial so unsolicited `OK init` means invoke-ready.
    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap).await.expect("MapRt::init");
    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }];
    let (mut rt, report) = XdpRt::init(
        flash,
        &slots,
        exec_bench::mem_map::TABLE,
        &mut maps,
        exec_bench::mem_map::HELPER_DISPATCHER_BASE,
    )
    .await
    .expect("XdpRt::init");

    let n_images = report
        .iter()
        .filter(|i| matches!(i, ReportItem::Xdp(_)))
        .count();
    info!("xdp init images={n_images}");
    for item in &report {
        match item {
            ReportItem::Xdp(name) => info!("report xdp {name}"),
            ReportItem::Map(ty, name) => info!("report map {ty:?} {name}"),
        }
    }
    if n_images > 0 {
        match rt.switch(0, 0, Some(&maps)) {
            Ok(()) => match rt.lease(Some(&mut maps)) {
                Ok(()) => info!("xdp leased exec_bench"),
                Err(e) => error!("xdp lease failed: {e:?}"),
            },
            Err(e) => error!("xdp switch failed: {e:?}"),
        }
    } else {
        error!("xdp cold empty — flash a workload xdp into the xdp partition");
    }
    drop(rt);
    drop(maps);

    spawner.spawn(control_serial(usb_rx).unwrap());

    loop {
        Timer::after(Duration::from_secs(60)).await;
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
        .on_enable_exec(|reply: ReplyWriter<'_>| match run_exec() {
            Ok(()) => reply.ok(OkReply::EnableExec),
            Err(e) => reply_map_err(reply, e),
        })
        .on_disable_exec(|reply: ReplyWriter<'_>| match app_ctrl::disable_xdp() {
            // Exec shares the leased image; disable releases the lease.
            Ok(()) => reply.ok(OkReply::DisableExec),
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
                match update_workload_or_default(name, key, value) {
                    Ok(()) => reply.ok(OkReply::UpdateMap),
                    Err(e) => reply_map_err(reply, e),
                }
            },
        )
        .on_clear_perf(|reply: ReplyWriter<'_>| {
            xpost::ext::perf::clear_perf();
            reply.ok(OkReply::ClearPerf);
        })
        .on_get_perf(|reply: ReplyWriter<'_>| {
            let n = xpost::ext::perf::report_payload_len();
            reply.get_perf(n, |write| {
                xpost::ext::perf::report_perf(write);
            });
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

fn parse_key01(key: &[u8]) -> Option<u8> {
    match key {
        b"0" => Some(KEY_INPUT),
        b"1" => Some(KEY_OUTPUT),
        [b] if *b == KEY_INPUT || *b == KEY_OUTPUT => Some(*b),
        _ => None,
    }
}

/// `workload` accepts raw value tokens (padded/truncated to 32 bytes) with key
/// `0`/`1`. Other maps fall back to [`app_ctrl::update_map`].
fn update_workload_or_default(name: &str, key: &[u8], value: &[u8]) -> Result<(), AppMapError> {
    if name != MAP_NAME {
        return app_ctrl::update_map(name, key, value);
    }
    let slot = parse_key01(key).ok_or(AppMapError::InvalidKey)?;
    let (mut maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
    let vs = maps.hash_value_size(idx)?;
    if vs != WORKLOAD_VALUE_LEN {
        return Err(AppMapError::Unsupported);
    }
    let mut val = [0u8; WORKLOAD_VALUE_LEN];
    let n = value.len().min(WORKLOAD_VALUE_LEN);
    val[..n].copy_from_slice(&value[..n]);
    maps.hash_update(idx, &[slot], &val, BPF_ANY)
        .map_err(AppMapError::from)
}

fn run_exec() -> Result<(), AppMapError> {
    // Ensure the compute image is leased before invoke.
    app_ctrl::enable_xdp().map_err(|e| match e {
        AppXdpError::Busy => AppMapError::Busy,
        AppXdpError::NoRt => AppMapError::NoRt,
        AppXdpError::Logic => AppMapError::Logic,
    })?;

    let buf: &mut [u8; BUF_CAP] = unsafe { &mut *addr_of_mut!(BENCH_BUF) };
    buf.fill(0);

    {
        let (maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
        let mut tmp = [0u8; WORKLOAD_VALUE_LEN];
        let n = maps
            .hash_lookup(idx, &[KEY_INPUT], &mut tmp)
            .map_err(|e| match e {
                xpost::map::MapUseError::NotFound => AppMapError::InvalidKey,
                other => AppMapError::from(other),
            })?;
        let copy = n.min(BUF_CAP).min(WORKLOAD_VALUE_LEN);
        buf[..copy].copy_from_slice(&tmp[..copy]);
    }

    let (act, _) = xpost::ext::exec_call::invoke(buf);
    if act == xpost_type::XdpAction::Aborted {
        return Err(AppMapError::Logic);
    }

    {
        let (mut maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
        let mut out = [0u8; WORKLOAD_VALUE_LEN];
        out.copy_from_slice(&buf[..WORKLOAD_VALUE_LEN]);
        maps.hash_update(idx, &[KEY_OUTPUT], &out, BPF_ANY)
            .map_err(AppMapError::from)?;
    }
    Ok(())
}
