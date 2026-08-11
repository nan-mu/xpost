// use embedded_storage::Storage;
use core::ptr;
use esp_hal::peripherals::FLASH;
// use esp_storage::FlashStorage;
use log::info;

use super::helper_dispatcher;
use super::mem_map::{HOT_A_LEN, assert_linked_addresses};
use super::prog_table::{self, SlotId};
use super::redirect::{clear_pending, xdp_action_name};

/// Firmware-owned HOT_A placeholder (RX default entry).
#[unsafe(link_section = ".iram1.hotload_a")]
#[unsafe(no_mangle)]
static mut HOT_A: [u8; HOT_A_LEN] = [0u8; HOT_A_LEN];

/// Firmware-owned HOT_B placeholder (hot-replace / tail_call target).
#[unsafe(link_section = ".iram1.hotload_b")]
#[unsafe(no_mangle)]
static mut HOT_B: [u8; super::mem_map::HOT_B_LEN] = [0u8; super::mem_map::HOT_B_LEN];

/// IRAM buffer pointer for a hot slot (for promote / inspect).
pub fn slot_buf(slot: SlotId) -> (*mut u8, usize) {
    match slot {
        SlotId::A => (ptr::addr_of_mut!(HOT_A) as *mut u8, HOT_A_LEN),
        SlotId::B => (
            ptr::addr_of_mut!(HOT_B) as *mut u8,
            super::mem_map::HOT_B_LEN,
        ),
    }
}

pub fn load(_flash: FLASH<'static>) {
    assert_linked_addresses(
        helper_dispatcher as *const () as usize,
        ptr::addr_of!(HOT_A) as usize,
        ptr::addr_of!(HOT_B) as usize,
    );

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

        info!(
            "[AOT] Bin: {} bytes, BSS: {} bytes (HOT_A ok)",
            bin_size, bss_size
        );

        let xdp_ptr = ptr::addr_of_mut!(HOT_A) as *mut u8;
        ptr::copy_nonoverlapping(xdp_with_footer.as_ptr(), xdp_ptr, bin_size);
        ptr::write_bytes(xdp_ptr.add(bin_size), 0, bss_size);

        info!("[AOT] load HOT_A done");
    }

    prog_table::mark_loaded_len(SlotId::A, bin_size);
    super::map::bind_aot_map_addrs();
    info!("[AOT] map aliases bound={}", super::map::AOT_MAP_BIND_COUNT);
}

pub use super::ctx::XdpContext;

/// Borrow the loaded IRAM image (or full window if marked loaded without precise len).
pub fn read_slot(slot: SlotId) -> Option<&'static [u8]> {
    let len = prog_table::image_len(slot);
    if len == 0 {
        return None;
    }
    let (ptr, _cap) = slot_buf(slot);
    Some(unsafe { core::slice::from_raw_parts(ptr as *const u8, len) })
}

/// Copy `image` into an IRAM hot slot and mark it loaded.
///
/// Caller must hold the control-plane gate (`datapath` stopped / `ctrl_try_begin`).
pub fn write_slot(slot: SlotId, image: &[u8]) -> Result<(), ()> {
    let (dst, cap) = slot_buf(slot);
    if image.len() > cap {
        return Err(());
    }
    unsafe {
        ptr::copy_nonoverlapping(image.as_ptr(), dst, image.len());
        if image.len() < cap {
            ptr::write_bytes(dst.add(image.len()), 0, cap - image.len());
        }
        core::arch::asm!("fence.i");
    }
    prog_table::mark_loaded_len(slot, image.len());
    Ok(())
}

pub fn xdp(data: &mut [u8]) -> i32 {
    let xdp_ptr = ptr::addr_of_mut!(HOT_A) as *mut u8;

    unsafe {
        core::arch::asm!("fence.i");

        clear_pending();
        prog_table::reset_tail_depth();
        let mut ctx = super::ctx::XdpMd::from_buf(data);

        // Decimal pointer values — hex triggers espflash monitor addr2line spam.
        let data_addr = ctx.data as usize;
        let data_end_addr = ctx.data_end as usize;
        info!(
            "[XDP] begin data={} data_end={} len={}",
            data_addr,
            data_end_addr,
            data_end_addr.saturating_sub(data_addr)
        );

        let ctx_ptr = &mut ctx as *mut XdpContext;
        let frame = core::ptr::addr_of_mut!(prog_table::XDP_FRAME);

        let res: i32;
        // Save SP + continuation RA so bpf_tail_call can unwind to this frame
        // and transfer to another program that still returns here.
        core::arch::asm!(
            "mv t0, sp",
            "lla t1, 1f",
            "sw t0, 0({frame})",
            "sw t1, 4({frame})",
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

        info!("[XDP] end action={} ({})", xdp_action_name(res), res);
        res
    }
}
