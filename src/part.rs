use core::ptr;

use crate::{bench_log, init_log, trace_log};

use super::helper_dispatcher;
use super::mem_map::assert_linked_addresses;
use super::mount;
use super::prog_table::{self, SlotId};
use super::redirect::{XDP_PASS, clear_pending, xdp_action_name};

include!(concat!(env!("OUT_DIR"), "/hot_slots.rs"));

/// Verify helper + hot-slot link addresses (safe to call with empty slots).
pub fn check_layout() {
    assert_linked_addresses(helper_dispatcher as *const () as usize, &slot_link_addrs());
}

/// Copy a boot AOT image into the `rx` mount slot and bind map aliases.
///
/// Caller supplies the bytes (firmware `include_bytes!` of `esp32c3.bin`).
pub fn load_boot_image(image: &[u8]) {
    check_layout();

    let bin_size = image.len();

    let Some(slot) = mount::try_rx_slot() else {
        init_log!("[AOT] no mount=rx; skip load");
        return;
    };

    let cap = slot.len();
    unsafe {
        let bss_size = 0;

        assert!(
            bin_size + bss_size <= cap,
            "payload {}+{} exceeds slot{} len {}",
            bin_size,
            bss_size,
            slot.index(),
            cap
        );

        init_log!(
            "[AOT] Bin: {} bytes, BSS: {} bytes (slot{} ok)",
            bin_size,
            bss_size,
            slot.index()
        );

        let (xdp_ptr, _cap) = slot_buf(slot.index());
        ptr::copy_nonoverlapping(image.as_ptr(), xdp_ptr, bin_size);
        ptr::write_bytes(xdp_ptr.add(bin_size), 0, bss_size);

        init_log!("[AOT] load slot{} done", slot.index());
    }

    prog_table::mark_loaded_len(slot, bin_size);
    super::map::bind_aot_map_addrs();
    let mut claimed = 0usize;
    for name in super::map::AOT_MAP_USER_NAMES {
        let Some(id) = super::map::resolve_map_name(name) else {
            init_log!("[AOT] add_user skip unknown map {name}");
            continue;
        };
        if super::map::pool::RawResource::add_user_sync(id) {
            claimed += 1;
        } else {
            init_log!("[AOT] add_user failed name={name} id={id}");
        }
    }
    init_log!(
        "[AOT] map aliases bound={} users+={}",
        super::map::AOT_MAP_BIND_COUNT,
        claimed
    );
}

pub use super::ctx::XdpContext;

/// Borrow the loaded IRAM image (or full window if marked loaded without precise len).
pub fn read_slot(slot: SlotId) -> Option<&'static [u8]> {
    let len = prog_table::image_len(slot);
    if len == 0 {
        return None;
    }
    let (ptr, _cap) = slot_buf(slot.index());
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
}

/// Copy `image` into an IRAM hot slot and mark it loaded.
///
/// Caller must hold the control-plane gate (`datapath` stopped / `ctrl_try_begin`).
///
/// Timing uses Embassy's TIMG clock — ESP32-C3 does not implement RISC-V `mcycle`
/// (CSR 0xB00 traps as illegal instruction).
pub fn write_slot(slot: SlotId, image: &[u8]) -> Result<(), ()> {
    let (dst, cap) = slot_buf(slot.index());
    if image.len() > cap {
        return Err(());
    }
    let t0 = embassy_time::Instant::now();
    unsafe {
        ptr::copy_nonoverlapping(image.as_ptr(), dst, image.len());
        if image.len() < cap {
            ptr::write_bytes(dst.add(image.len()), 0, cap - image.len());
        }
        #[cfg(target_arch = "riscv32")]
        core::arch::asm!("fence.i");
    }
    prog_table::mark_loaded_len(slot, image.len());
    let us = (embassy_time::Instant::now() - t0).as_micros();
    bench_log!(
        "[HOT] swap slot={} bytes={} us={}",
        slot.index(),
        image.len(),
        us
    );
    Ok(())
}

#[cfg(not(target_arch = "riscv32"))]
fn invoke_slot_md(slot: SlotId, ctx: &mut super::ctx::XdpMd) -> i32 {
    let _ = (slot, ctx);
    XDP_PASS
}

/// RV32IMC `jalr` into the IRAM slot. ESP32-C3 first instantiation only.
#[cfg(target_arch = "riscv32")]
fn invoke_slot_md(slot: SlotId, ctx: &mut super::ctx::XdpMd) -> i32 {
    if !prog_table::is_loaded(slot) {
        return XDP_PASS;
    }

    let (xdp_ptr, _) = slot_buf(slot.index());

    unsafe {
        core::arch::asm!("fence.i");

        clear_pending();
        prog_table::reset_tail_depth();

        let data_addr = ctx.data as usize;
        let data_end_addr = ctx.data_end as usize;
        trace_log!(
            "[XDP] begin slot={} data={} data_end={} len={}",
            slot.index(),
            data_addr,
            data_end_addr,
            data_end_addr.saturating_sub(data_addr)
        );

        let ctx_ptr = ctx as *mut XdpContext;
        let frame = core::ptr::addr_of_mut!(prog_table::XDP_FRAME);

        let res: i32;
        core::arch::asm!(
            "mv t0, sp",
            "lla t1, 1f",
            "sw t0, 0({frame})",
            "sw t1, 4({frame})",
            "sw s0, 8({frame})",
            "sw s1, 12({frame})",
            "sw s2, 16({frame})",
            "sw s3, 20({frame})",
            "sw s4, 24({frame})",
            "sw s5, 28({frame})",
            "sw s6, 32({frame})",
            "sw s7, 36({frame})",
            "sw s8, 40({frame})",
            "sw s9, 44({frame})",
            "sw s10, 48({frame})",
            "sw s11, 52({frame})",
            "jalr {entry}",
            "1:",
            frame = in(reg) frame,
            entry = in(reg) xdp_ptr,
            in("a0") ctx_ptr,
            lateout("a0") res,
            out("t0") _,
            out("t1") _,
            clobber_abi("C"),
        );

        trace_log!(
            "[XDP] end slot={} action={} ({})",
            slot.index(),
            xdp_action_name(res),
            res
        );
        res
    }
}

fn invoke_slot(slot: SlotId, data: &mut [u8]) -> i32 {
    if !prog_table::is_loaded(slot) {
        super::ctx::clear_window();
        return XDP_PASS;
    }
    let mut ctx = super::ctx::XdpMd::from_buf(data);
    let res = invoke_slot_md(slot, &mut ctx);
    super::ctx::record_window(&ctx);
    res
}

/// RX hook: AOT runs **in place** on the Wi-Fi DMA slice so `XDP_TX` can
/// `tx_by_ref` the same buffer. `adjust_tail` shrink is visible via
/// [`super::ctx::clip_to_md_window`]. Scratch is not used here (`adjust_head`
/// grow still needs scratch and is not on the zero-copy TX path).
pub fn xdp(data: &mut [u8]) -> i32 {
    let Some(slot) = mount::try_rx_slot() else {
        super::ctx::clear_window();
        return XDP_PASS;
    };
    invoke_slot(slot, data)
}
