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
use xpost::app_ctrl;
use xpost::map::{HEAP_SIZE, MapRt};
use xpost::xdp::{ReportItem, Slot, XdpRt};
use xpost_serial::reply::{MapError, OkReply, PerfError, ServerError};
use xpost_serial::server::reply::{write_error, write_ok};
use xpost_serial::r#type::command::Command;

use exec_bench_common::{self as bench, BUF_CAP, OPCODES};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

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
    if n_images > 0 {
        match rt.switch(0, 0, Some(&maps)) {
            Ok(()) => match rt.lease(Some(&mut maps)) {
                Ok(()) => info!("xdp leased exec_bench"),
                Err(e) => error!("xdp lease failed: {e:?}"),
            },
            Err(e) => error!("xdp switch failed: {e:?}"),
        }
    } else {
        error!("xdp cold empty — flash exec_bench.elf into the xdp partition");
    }
    drop(rt);
    drop(maps);

    spawner.spawn(control_serial(usb_rx).unwrap());

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

/// App serial: frozen verbs via `Command` + experiment-local `bench …` lines.
///
/// `bench *` is **not** in the frozen App wire; agent `/v1/run` cannot send it.
/// Use raw UART (monitor) or a future wire extension. See `HOST_DESIGN.md`.
#[embassy_executor::task]
async fn control_serial(
    mut rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
) {
    let mut write = |bytes: &[u8]| -> Result<(), Infallible> {
        esp_println::Printer::write_bytes(bytes);
        Ok(())
    };
    let _ = write_ok(&OkReply::Init, &mut write);

    let mut line = [0u8; 128];
    let mut used = 0usize;
    let mut overflow = false;
    loop {
        let mut idle = true;
        for _ in 0..64 {
            let Ok(byte) = rx.read_byte() else {
                break;
            };
            idle = false;
            if byte == b'\n' || byte == b'\r' {
                if overflow {
                    let _ = write(b"ERR line-too-long\n");
                    used = 0;
                    overflow = false;
                } else if used != 0 {
                    dispatch_line(&line[..used], &mut write);
                    used = 0;
                }
            } else if !overflow {
                if let Some(slot) = line.get_mut(used) {
                    *slot = byte;
                    used += 1;
                } else {
                    overflow = true;
                }
            }
        }
        Timer::after(Duration::from_millis(if idle { 10 } else { 1 })).await;
    }
}

fn looks_like_bench(line: &[u8]) -> bool {
    let Ok(s) = core::str::from_utf8(line) else {
        return false;
    };
    let s = s.trim();
    s.eq_ignore_ascii_case("bench list") || {
        let lower = s.as_bytes();
        lower.len() >= 6
            && lower[..5].eq_ignore_ascii_case(b"bench")
            && lower[5] == b' '
    }
}

fn dispatch_line(line: &[u8], write: &mut impl FnMut(&[u8]) -> Result<(), Infallible>) {
    if looks_like_bench(line) {
        handle_bench(line, write);
        return;
    }
    match Command::try_from(line) {
        Ok(Command::Ping) => {
            let _ = write_ok(&OkReply::Ping, write);
        }
        Ok(Command::EnableXdp) => match app_ctrl::enable_xdp() {
            Ok(()) => {
                let _ = write_ok(&OkReply::EnableXdp, write);
            }
            Err(_) => {
                let _ = write_error(&ServerError::Logic, write);
            }
        },
        Ok(Command::DisableXdp) => match app_ctrl::disable_xdp() {
            Ok(()) => {
                let _ = write_ok(&OkReply::DisableXdp, write);
            }
            Err(_) => {
                let _ = write_error(&ServerError::Logic, write);
            }
        },
        Ok(Command::ClearMap { .. } | Command::GetMap { .. } | Command::UpdateMap { .. }) => {
            let _ = write_error(&MapError::NameNotFound, write);
        }
        Ok(Command::ClearPerf | Command::GetPerf) => {
            let _ = write_error(&PerfError::Unavailable, write);
        }
        Ok(Command::GetInit) => {
            let _ = write_error(&ServerError::Unknown, write);
        }
        Err(e) => {
            let _ = write_error(&e, write);
        }
    }
}

fn handle_bench(line: &[u8], write: &mut impl FnMut(&[u8]) -> Result<(), Infallible>) {
    let Ok(text) = core::str::from_utf8(line) else {
        let _ = write(b"ERR unknown\n");
        return;
    };
    let text = text.trim();
    if text.eq_ignore_ascii_case("bench list") {
        for (name, op) in OPCODES {
            let _ = write_fmt(write, *name, *op);
        }
        let _ = write(b"OK bench list end\n");
        return;
    }
    if let Some(rest) = text.strip_prefix("bench run ") {
        let mut parts = rest.split_whitespace();
        let (Some(op_name), Some(iters_s)) = (parts.next(), parts.next()) else {
            let _ = write(b"ERR bench run <op> <iters>\n");
            return;
        };
        let Some(op) = bench::parse_opcode(op_name) else {
            let _ = write(b"ERR unknown opcode\n");
            return;
        };
        let Ok(iters) = iters_s.parse::<u32>() else {
            let _ = write(b"ERR bad iters\n");
            return;
        };
        if iters == 0 {
            let _ = write(b"ERR iters must be > 0\n");
            return;
        }
        let buf: &mut [u8; BUF_CAP] = unsafe { &mut *addr_of_mut!(BENCH_BUF) };
        let (cycles, us, ok) = bench::time_loop(buf, op, iters, |b| {
            let (act, _) = xpost::ext::exec_call::invoke(b);
            if act == xpost_type::XdpAction::Aborted {
                Err(bench::BenchError::Aborted)
            } else {
                Ok(())
            }
        });
        let name = bench::opcode_name(op).unwrap_or("?");
        write_bench_result(write, name, iters, cycles, us, ok);
        return;
    }
    if let Some(rest) = text.strip_prefix("bench verify ") {
        let Some(op) = bench::parse_opcode(rest.trim()) else {
            let _ = write(b"ERR unknown opcode\n");
            return;
        };
        let buf: &mut [u8; BUF_CAP] = unsafe { &mut *addr_of_mut!(BENCH_BUF) };
        let ok = bench::prepare(buf, op).is_ok()
            && {
                let (act, _) = xpost::ext::exec_call::invoke(buf);
                act != xpost_type::XdpAction::Aborted
            }
            && bench::verify(buf);
        let name = bench::opcode_name(op).unwrap_or("?");
        let _ = write(b"OK bench verify ");
        let _ = write(name.as_bytes());
        let _ = write(if ok { b" ok=1\n" } else { b" ok=0\n" });
        return;
    }
    let _ = write(b"ERR unknown cmd\n");
}

fn write_fmt(write: &mut impl FnMut(&[u8]) -> Result<(), Infallible>, name: &str, op: u32) {
    let _ = write(b"OK bench list ");
    let _ = write(name.as_bytes());
    let _ = write(b" ");
    let mut buf = [0u8; 20];
    let _ = write(u32_dec(op, &mut buf));
    let _ = write(b"\n");
}

fn write_bench_result(
    write: &mut impl FnMut(&[u8]) -> Result<(), Infallible>,
    name: &str,
    iters: u32,
    cycles: u64,
    us: u64,
    ok: bool,
) {
    let _ = write(b"OK bench op=");
    let _ = write(name.as_bytes());
    let _ = write(b" iters=");
    let mut b = [0u8; 20];
    let _ = write(u32_dec(iters, &mut b));
    let _ = write(b" cycles=");
    let _ = write(u64_dec(cycles, &mut b));
    let _ = write(b" us=");
    let _ = write(u64_dec(us, &mut b));
    let _ = write(if ok { b" ok=1\n" } else { b" ok=0\n" });
}

fn u32_dec(mut n: u32, buf: &mut [u8; 20]) -> &[u8] {
    if n == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[i..]
}

fn u64_dec(mut n: u64, buf: &mut [u8; 20]) -> &[u8] {
    if n == 0 {
        buf[0] = b'0';
        return &buf[..1];
    }
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    &buf[i..]
}
