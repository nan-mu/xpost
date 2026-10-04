#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
)]
#![deny(clippy::large_stack_frames)]

//! Native HAL twin of `exec_bench`: same host pipeline (map / enable exec /
//! `OK finish` / perf), no XDP lease. Workload bytes are opaque at the serial
//! boundary; [`exec_bench_common::run_hal`] interprets the buffer.

use core::cell::RefCell;
use core::convert::Infallible;
use core::ptr::addr_of_mut;

use critical_section::Mutex;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use heapless::FnvIndexMap;
use log::error;
use xpost::app_ctrl;
use xpost::xdp::Slot;
use xpost_serial::reply::{MapError, OkReply, ServerError};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

use exec_bench_common::{self as bench, BUF_CAP};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

const MAP_NAME: &str = "workload";
const VALUE_LEN: usize = 32;
const KEY_IN: u8 = 0;
const KEY_OUT: u8 = 1;
const MAP_CAP: u32 = 2;

/// Slot metadata so `xpost` links; HAL never leases a cold image.
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<0xE00> = Slot::new([0u8; 0xE00]);

static WORKLOAD: Mutex<RefCell<FnvIndexMap<u8, [u8; VALUE_LEN], 2>>> =
    Mutex::new(RefCell::new(FnvIndexMap::new()));

static mut RUN_BUF: [u8; BUF_CAP] = [0u8; BUF_CAP];

#[allow(
    clippy::large_stack_frames,
    reason = "embassy main allocates larger boot buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Error);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let _ = &xdp_slot;

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
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
        .on_enable_xdp(|reply: ReplyWriter<'_>| reply.error(&ServerError::Unsupported))
        .on_disable_xdp(|reply: ReplyWriter<'_>| reply.error(&ServerError::Unsupported))
        .on_enable_exec(|reply: ReplyWriter<'_>| match run_exec() {
            Ok(()) => reply.ok(OkReply::EnableExec),
            Err(e) => reply.error(&e),
        })
        .on_disable_exec(|reply: ReplyWriter<'_>| {
            // No lease to release; keep verb symmetry with the eBPF arm.
            reply.ok(OkReply::DisableExec);
        })
        .on_clear_map(|name: &str, reply: ReplyWriter<'_>| {
            if name != MAP_NAME {
                reply.error(&MapError::NameNotFound);
                return;
            }
            critical_section::with(|cs| WORKLOAD.borrow(cs).borrow_mut().clear());
            reply.ok(OkReply::ClearMap);
        })
        .on_get_map(|name: &str, reply: ReplyWriter<'_>| {
            if name != MAP_NAME {
                reply.error(&MapError::NameNotFound);
                return;
            }
            critical_section::with(|cs| {
                let map = WORKLOAD.borrow(cs).borrow();
                let size = map.len() as u32;
                let mut len = app_ctrl::hash_header_len(size, MAP_CAP);
                for (k, v) in map.iter() {
                    len += app_ctrl::hash_entry_len(&[*k], v);
                }
                reply.get_map(len, |write| {
                    let mut emit = |chunk: &[u8]| {
                        let _ = write(chunk);
                    };
                    app_ctrl::write_hash_header(size, MAP_CAP, &mut emit);
                    for (k, v) in map.iter() {
                        app_ctrl::write_hash_entry(&[*k], v, &mut emit);
                    }
                });
            });
        })
        .on_update_map(
            |name: &str, key: &[u8], value: &[u8], reply: ReplyWriter<'_>| {
                match update_bytes(name, key, value) {
                    Ok(()) => reply.ok(OkReply::UpdateMap),
                    Err(e) => reply.error(&e),
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

fn key_u8(key: &[u8]) -> Option<u8> {
    match key {
        b"0" => Some(0),
        b"1" => Some(1),
        [0] | [1] => Some(key[0]),
        _ => None,
    }
}

fn update_bytes(name: &str, key: &[u8], value: &[u8]) -> Result<(), MapError> {
    if name != MAP_NAME {
        return Err(MapError::NameNotFound);
    }
    let slot = key_u8(key).ok_or(MapError::InvalidKey)?;
    let mut val = [0u8; VALUE_LEN];
    let n = value.len().min(VALUE_LEN);
    val[..n].copy_from_slice(&value[..n]);
    let ok = critical_section::with(|cs| {
        WORKLOAD.borrow(cs).borrow_mut().insert(slot, val).is_ok()
    });
    if ok {
        Ok(())
    } else {
        Err(MapError::Full)
    }
}

/// Same pipeline step as eBPF `enable exec`: map[0] → compute → map[1], then
/// raw UART `OK finish` (twin of eBPF `bpf_printk`; not a serial reply).
fn run_exec() -> Result<(), MapError> {
    let mut in_val = [0u8; VALUE_LEN];
    let found = critical_section::with(|cs| {
        let map = WORKLOAD.borrow(cs).borrow();
        match map.get(&KEY_IN) {
            Some(v) => {
                in_val.copy_from_slice(v);
                true
            }
            None => false,
        }
    });
    if !found {
        return Err(MapError::KeyNotFound);
    }

    let buf: &mut [u8; BUF_CAP] = unsafe { &mut *addr_of_mut!(RUN_BUF) };
    buf.fill(0);
    buf[..VALUE_LEN].copy_from_slice(&in_val);

    if bench::run_hal(buf).is_err() {
        return Err(MapError::InvalidValue);
    }

    let mut out = [0u8; VALUE_LEN];
    out.copy_from_slice(&buf[..VALUE_LEN]);
    let ok = critical_section::with(|cs| {
        WORKLOAD.borrow(cs).borrow_mut().insert(KEY_OUT, out).is_ok()
    });
    if !ok {
        return Err(MapError::Full);
    }

    // Unsolicited transcript marker for host `GetFinish` (not OkReply::Finish).
    esp_println::Printer::write_bytes(b"OK finish\n");
    Ok(())
}
