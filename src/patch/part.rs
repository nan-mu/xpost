// use embedded_storage::Storage;
use core::ptr;
use esp_hal::peripherals::FLASH;

use crate::{bench_log, init_log, trace_log};

use super::helper_dispatcher;
use super::mem_map::{HOT_A_LEN, assert_linked_addresses};
use super::prog_table::{self, SlotId};
use super::redirect::{clear_pending, xdp_action_name};

include!(concat!(env!("OUT_DIR"), "/hot_slots.rs"));

/// Verify helper + hot-slot link addresses (safe to call with empty slots).
pub fn check_layout() {
    assert_linked_addresses(helper_dispatcher as *const () as usize, &slot_link_addrs());
}

pub fn load(_flash: FLASH<'static>) {
    check_layout();

    let xdp_with_footer = include_bytes!("/Users/nan/bs/firmware/esp32c3.bin");
    let bin_size = xdp_with_footer.len();

    unsafe {
        let bss_size = 0;

        assert!(
            bin_size + bss_size <= HOT_A_LEN,
            "payload {}+{} exceeds HOT_A_LEN {}",
            bin_size,
            bss_size,
            HOT_A_LEN
        );

        init_log!(
            "[AOT] Bin: {} bytes, BSS: {} bytes (slot0 ok)",
            bin_size,
            bss_size
        );

        let (xdp_ptr, _cap) = slot_buf(0);
        ptr::copy_nonoverlapping(xdp_with_footer.as_ptr(), xdp_ptr, bin_size);
        ptr::write_bytes(xdp_ptr.add(bin_size), 0, bss_size);

        init_log!("[AOT] load slot0 done");
    }

    prog_table::mark_loaded_len(SlotId::A, bin_size);
    super::map::bind_aot_map_addrs();
    init_log!("[AOT] map aliases bound={}", super::map::AOT_MAP_BIND_COUNT);
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

pub fn xdp(data: &mut [u8]) -> i32 {
    // RX entry is always hot slot 0. Empty IRAM must not jalr (SoftFault).
    if !prog_table::is_loaded(SlotId::A) {
        return 2; // XDP_PASS
    }

    let (xdp_ptr, _) = slot_buf(0);

    unsafe {
        core::arch::asm!("fence.i");

        clear_pending();
        prog_table::reset_tail_depth();
        let mut ctx = super::ctx::XdpMd::from_buf(data);

        // Decimal pointer values — hex triggers espflash monitor addr2line spam.
        let data_addr = ctx.data as usize;
        let data_end_addr = ctx.data_end as usize;
        trace_log!(
            "[XDP] begin data={} data_end={} len={}",
            data_addr,
            data_end_addr,
            data_end_addr.saturating_sub(data_addr)
        );

        let ctx_ptr = &mut ctx as *mut XdpContext;
        let frame = core::ptr::addr_of_mut!(prog_table::XDP_FRAME);

        let res: i32;
        // Save SP + continuation RA + callee-saved s0–s11 so bpf_tail_call can
        // unwind past the helper/A frames and transfer to another program that
        // still returns here with a valid Rust frame pointer.
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

        trace_log!("[XDP] end action={} ({})", xdp_action_name(res), res);
        res
    }
}
