#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
)]
#![deny(clippy::large_stack_frames)]

//! Opaque carrier for function-call XDP: lease an image, bridge map bytes into
//! [`xpost::ext::exec_call::invoke`], and expose `SerialHandler` map/perf/exec
//! verbs. This App does **not** interpret workload opcodes or buffer layouts —
//! those live in the XDP image and in host / xpost-test parsing.

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

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

/// Map value size for `workload` (XDP ELF metadata from libbpf-mcu).
const VALUE_LEN: usize = 32;
const MAP_NAME: &str = "workload";
const KEY_IN: u8 = 0;
const KEY_OUT: u8 = 1;

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

static mut INVOKE_BUF: [u8; VALUE_LEN] = [0u8; VALUE_LEN];

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
    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }, unsafe {
        xdp_slot_idle.as_xdp_slot(false)
    }];
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
                Ok(()) => info!("xdp leased"),
                Err(e) => error!("xdp lease failed: {e:?}"),
            },
            Err(e) => error!("xdp switch failed: {e:?}"),
        }
    } else {
        error!("xdp cold empty — flash an xdp image into the xdp partition");
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
                match update_bytes(name, key, value) {
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

fn key_u8(key: &[u8]) -> Option<u8> {
    match key {
        b"0" => Some(0),
        b"1" => Some(1),
        [0] | [1] => Some(key[0]),
        _ => None,
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Decode `VALUE_LEN*2` hex digits into 32 raw bytes; otherwise pad/truncate copy.
fn decode_value_token(value: &[u8]) -> Option<[u8; VALUE_LEN]> {
    let mut val = [0u8; VALUE_LEN];
    if value.len() == VALUE_LEN * 2 {
        for i in 0..VALUE_LEN {
            let hi = hex_nibble(value[i * 2])?;
            let lo = hex_nibble(value[i * 2 + 1])?;
            val[i] = (hi << 4) | lo;
        }
        return Some(val);
    }
    let n = value.len().min(VALUE_LEN);
    val[..n].copy_from_slice(&value[..n]);
    Some(val)
}

/// Opaque HASH update: wire `key`/`value` tokens are raw bytes (or hex for 32B).
/// Other maps fall back to [`app_ctrl::update_map`].
fn update_bytes(name: &str, key: &[u8], value: &[u8]) -> Result<(), AppMapError> {
    if name != MAP_NAME {
        return app_ctrl::update_map(name, key, value);
    }
    let slot = key_u8(key).ok_or(AppMapError::InvalidKey)?;
    let (mut maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
    let vs = maps.hash_value_size(idx)?;
    if vs != VALUE_LEN {
        return Err(AppMapError::Unsupported);
    }
    let val = decode_value_token(value).ok_or(AppMapError::InvalidValue)?;
    maps.hash_update(idx, &[slot], &val, BPF_ANY)
        .map_err(AppMapError::from)
}

/// Carrier path: lease → copy map[0] bytes → invoke → copy bytes to map[1].
/// No opcode / layout interpretation. `OK finish` comes from the XDP image
/// (`bpf_printk`), not from this App.
fn run_exec() -> Result<(), AppMapError> {
    app_ctrl::enable_xdp().map_err(|e| match e {
        AppXdpError::Busy => AppMapError::Busy,
        AppXdpError::NoRt => AppMapError::NoRt,
        AppXdpError::Logic => AppMapError::Logic,
    })?;

    let buf: &mut [u8; VALUE_LEN] = unsafe { &mut *addr_of_mut!(INVOKE_BUF) };
    buf.fill(0);

    {
        let (maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
        let n = maps.hash_lookup(idx, &[KEY_IN], buf).map_err(|e| match e {
            xpost::map::MapUseError::NotFound => AppMapError::InvalidKey,
            other => AppMapError::from(other),
        })?;
        if n > VALUE_LEN {
            return Err(AppMapError::Logic);
        }
    }

    let (act, _) = xpost::ext::exec_call::invoke(buf);
    if act == xpost_type::XdpAction::Aborted {
        return Err(AppMapError::Logic);
    }

    {
        let (mut maps, idx) = app_ctrl::lock_map(MAP_NAME)?;
        maps.hash_update(idx, &[KEY_OUT], buf, BPF_ANY)
            .map_err(AppMapError::from)?;
    }
    Ok(())
}
