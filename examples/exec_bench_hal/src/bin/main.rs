#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
)]
#![deny(clippy::large_stack_frames)]

use core::ptr::addr_of_mut;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::error;

use exec_bench_common::{self as bench, BUF_CAP, OPCODES};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

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

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();
    spawner.spawn(serial_task(usb_rx).unwrap());
    Timer::after(Duration::from_millis(1200)).await;
    // Same ready token as the eBPF arm so host parsers stay identical.
    esp_println::println!("xpost ready exec_bench");

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

#[embassy_executor::task]
async fn serial_task(
    mut rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
) {
    let mut line = [0u8; 96];
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
    } else if line.eq_ignore_ascii_case("bench list") {
        for (name, op) in OPCODES {
            esp_println::println!("OK bench list {name} {op}");
        }
        esp_println::println!("OK bench list end");
    } else if let Some(rest) = line.strip_prefix("bench run ") {
        let mut parts = rest.split_whitespace();
        let Some(op_name) = parts.next() else {
            esp_println::println!("ERR bench run <op> <iters>");
            return;
        };
        let Some(iters_s) = parts.next() else {
            esp_println::println!("ERR bench run <op> <iters>");
            return;
        };
        let Some(op) = bench::parse_opcode(op_name) else {
            esp_println::println!("ERR unknown opcode");
            return;
        };
        let Ok(iters) = iters_s.parse::<u32>() else {
            esp_println::println!("ERR bad iters");
            return;
        };
        if iters == 0 {
            esp_println::println!("ERR iters must be > 0");
            return;
        }
        let buf: &mut [u8; BUF_CAP] = unsafe { &mut *addr_of_mut!(BENCH_BUF) };
        let (cycles, us, ok) = bench::time_loop(buf, op, iters, bench::run_hal);
        let name = bench::opcode_name(op).unwrap_or("?");
        esp_println::println!(
            "OK bench op={name} iters={iters} cycles={cycles} us={us} ok={}",
            if ok { 1 } else { 0 }
        );
    } else if let Some(rest) = line.strip_prefix("bench verify ") {
        let Some(op) = bench::parse_opcode(rest.trim()) else {
            esp_println::println!("ERR unknown opcode");
            return;
        };
        let buf: &mut [u8; BUF_CAP] = unsafe { &mut *addr_of_mut!(BENCH_BUF) };
        let ok = bench::prepare(buf, op).is_ok()
            && bench::run_hal(buf).is_ok()
            && bench::verify(buf);
        esp_println::println!(
            "OK bench verify {} ok={}",
            bench::opcode_name(op).unwrap_or("?"),
            if ok { 1 } else { 0 }
        );
    } else {
        esp_println::println!("ERR unknown cmd");
    }
}
