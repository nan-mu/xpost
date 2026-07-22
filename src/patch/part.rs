// use embedded_storage::Storage;
use core::ptr;
use esp_hal::peripherals::FLASH;
// use esp_storage::FlashStorage;
use log::info;

use super::helper_dispatcher;
use super::mem_map::{assert_linked_addresses, HOT_REGION_LEN};
use super::redirect::clear_pending;

/// Firmware-owned HOT_REGION placeholder. Linker places this at [`HOT_REGION_BASE`].
#[unsafe(link_section = ".iram1.hotload_area")]
#[unsafe(no_mangle)]
static mut HOT_REGION: [u8; HOT_REGION_LEN] = [0u8; HOT_REGION_LEN];

pub fn load(_flash: FLASH<'static>) {
    assert_linked_addresses(
        helper_dispatcher as *const () as usize,
        ptr::addr_of!(HOT_REGION) as usize,
    );

    let xdp_with_footer = include_bytes!("/Users/nan/bs/firmware/esp32c3.bin");

    unsafe {
        let bin_size = xdp_with_footer.len();
        let bss_size = 0;

        assert!(
            bin_size + bss_size <= HOT_REGION_LEN,
            "payload {}+{} exceeds HOT_REGION_LEN {}",
            bin_size,
            bss_size,
            HOT_REGION_LEN
        );

        info!(
            "[AOT] Bin: {} bytes, BSS: {} bytes (HOT_REGION ok)",
            bin_size, bss_size
        );

        let xdp_ptr = ptr::addr_of_mut!(HOT_REGION) as *mut u8;
        ptr::copy_nonoverlapping(xdp_with_footer.as_ptr(), xdp_ptr, bin_size);
        ptr::write_bytes(xdp_ptr.add(bin_size), 0, bss_size);

        info!("[AOT] load done");
    }

    super::map::bind_aot_map_addrs();
    info!(
        "[AOT] map aliases bound={}",
        super::map::AOT_MAP_BIND_COUNT
    );
}

pub use super::ctx::XdpContext;

pub fn xdp(data: &mut [u8]) -> i32 {
    let xdp_ptr = ptr::addr_of_mut!(HOT_REGION) as *mut u8;

    unsafe {
        core::arch::asm!("fence.i");

        clear_pending();
        let mut ctx = super::ctx::XdpMd::from_buf(data);

        let ctx_ptr = &mut ctx as *mut XdpContext;

        // Keep logs free of raw pointers (espflash addr2line spam).
        log::debug!("[AOT] XDP call len={}", data.len());

        let res: i32;
        core::arch::asm!(
            "jalr {entry}",
            entry = in(reg) xdp_ptr,
            in("a0") ctx_ptr,
            lateout("a0") res,
            clobber_abi("C"),
        );

        log::debug!("[AOT] XDP return={}", res);
        res
    }
}
