#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types"
)]
#![deny(clippy::large_stack_frames)]

//! Opaque carrier for function-call XDP. Pass the EXB2 buffer to the leased
//! image in one invocation.

#[cfg(feature = "diag")]
mod diagnostics;
#[path = "../exec_metrics.rs"]
mod exec_metrics;

use core::convert::Infallible;
use core::ptr::addr_of_mut;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use exec_bench::mem_map::SLOT_A_LEN;
use log::{error, info};
use xpost::app_ctrl::{self, AppXdpError};
use xpost::map::{HEAP_SIZE, MapRt};
use xpost::xdp::{ReportItem, Slot, XdpRt};
use xpost_serial::command::EXEC_BATCH_BYTES;
use xpost_serial::reply::{OkReply, ServerError};
use xpost_serial::server::exec_batch::{EXEC_BATCH_LINE_BYTES, ExecBatch};
use xpost_serial::server::{ReplyWriter, SerialHandler, serial_task};

#[panic_handler]
fn panic(panic_info: &core::panic::PanicInfo) -> ! {
    error!("{}", panic_info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

#[unsafe(link_section = ".bss")]
static mut MAP_HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

/// Primary RX slot. Loaded before lease; immutable while it is the entry.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".xdp.hotload_0")]
static xdp_slot: Slot<SLOT_A_LEN> = Slot::new([0u8; SLOT_A_LEN]);

static mut XDP_COPY_BUFFER: [u8; SLOT_A_LEN] = [0; SLOT_A_LEN];

static mut EXEC_BATCH: ExecBatch = ExecBatch::new();

#[allow(
    clippy::large_stack_frames,
    reason = "embassy main allocates larger boot buffers"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Info);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let flash = peripherals.FLASH;
    let boot_id = esp_hal::rng::Rng::new().random().max(1);
    exec_metrics::init(boot_id);
    xpost::xdp::set_boot_id(boot_id);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let (usb_rx, _usb_tx) = UsbSerialJtag::new(peripherals.USB_DEVICE).split();

    // XdpRt initialization requires a map runtime guard.
    let map_heap: &'static mut [u8] = unsafe { &mut *addr_of_mut!(MAP_HEAP) };
    let mut maps = MapRt::init(map_heap).await.expect("MapRt::init");
    let slots = [unsafe { xdp_slot.as_xdp_slot(true) }];
    let copy_buffer = unsafe { &mut *addr_of_mut!(XDP_COPY_BUFFER) };
    let (mut rt, report) = XdpRt::init_with_copy_buffer(
        flash,
        &slots,
        exec_bench::mem_map::TABLE,
        &mut maps,
        exec_bench::mem_map::HELPER_DISPATCHER_BASE,
        copy_buffer,
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
        #[cfg(feature = "diag")]
        let map_bindings = Some(&maps);
        #[cfg(not(feature = "diag"))]
        let map_bindings = None;
        match rt.switch(0, 0, map_bindings) {
            Ok(()) => info!("xdp switched, not leased"),
            Err(e) => error!("xdp switch failed: {e:?}"),
        }
    } else {
        error!("xdp cold empty — flash an xdp image into the xdp partition");
    }
    drop(rt);
    drop(maps);

    // The single serial task owns the staging buffer for the rest of this boot.
    let batch = unsafe { &mut *addr_of_mut!(EXEC_BATCH) };
    spawner.spawn(control_serial(usb_rx, batch, boot_id).unwrap());

    loop {
        Timer::after(Duration::from_secs(60)).await;
    }
}

serial_task! {
    fn control_serial(
        rx: esp_hal::usb_serial_jtag::UsbSerialJtagRx<'static, esp_hal::Blocking>,
        batch: &'static mut ExecBatch,
        boot_id: u32,
    ) {
        let mut rx = rx;
        let builder = SerialHandler::builder(
            move || match rx.read_byte() {
                Ok(b) => Some(b),
                Err(_) => None,
            },
            |bytes| {
                esp_println::Printer::write_bytes(bytes);
                Ok::<(), Infallible>(())
            },
            [0u8; EXEC_BATCH_LINE_BYTES],
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
        .on_exec_batch(move |command, reply| batch.handle(command, reply, boot_id, run_exec_batch));
        #[cfg(feature = "diag")]
        let builder = diagnostics::register(builder);
        exec_metrics::register(builder).build()
    }
}

fn run_exec_batch(buf: &mut [u8; EXEC_BATCH_BYTES]) -> Result<(u64, u64, u32), ServerError> {
    let publish = app_ctrl::enable_xdp_stamped().map_err(|error| match error {
        AppXdpError::Busy => ServerError::Busy,
        AppXdpError::NoRt | AppXdpError::Logic => ServerError::Logic,
    })?;
    exec_metrics::begin_invocation();
    let begin = exec_metrics::now();
    #[cfg(not(feature = "diag"))]
    let (action, _) = xpost::ext::exec_call::invoke(buf);
    // The lease-only path skips the diagnostic Trace hooks.
    #[cfg(feature = "diag")]
    let action = xpost::ext::exec_call::invoke_batch(buf, 1)
        .map_or(xpost_type::XdpAction::Aborted, |(_, action)| action);
    let end = exec_metrics::now();
    exec_metrics::end_invocation(action);
    if publish != 0 {
        if let Err(err) = app_ctrl::disable_xdp() {
            error!("exec batch lease release failed: {err:?}");
        }
    }
    Ok((begin, end, action as u32))
}

fn reply_xdp_err(reply: ReplyWriter<'_>, e: AppXdpError) {
    match e {
        AppXdpError::Busy => reply.error(&ServerError::Busy),
        AppXdpError::NoRt | AppXdpError::Logic => reply.error(&ServerError::Logic),
    }
}
