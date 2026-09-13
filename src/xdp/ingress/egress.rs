//! XDP_TX: zero-copy Wi-Fi transmit; on failure the frame is dropped (no bounce queue).

use core::ffi::c_void;

use crate::xdp::state::{ESP_OK, WIFI_IF_STA};

unsafe extern "C" {
    fn esp_wifi_internal_tx_by_ref(
        ifx: u32,
        buffer: *mut c_void,
        len: usize,
        netstack_buf: *mut c_void,
    ) -> i32;
    fn esp_wifi_internal_reg_netstack_buf_cb(
        ref_: Option<unsafe extern "C" fn(*mut c_void)>,
        free: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> i32;
}

/// Register no-op netstack ref/free so `tx_by_ref` can run without a copy path.
pub(super) fn arm_zero_copy() {
    let rc =
        unsafe { esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free)) };
    if rc != ESP_OK {
        log::warn!("[xdp][egress] reg_netstack_buf_cb rc={rc}; XDP_TX zc may fail and drop");
    }
}

/// Apply XDP_TX: clip to md window, zero-copy TX; failure drops the frame.
#[inline]
pub(super) fn transmit(frame: &mut [u8]) {
    let clipped = crate::ctx::clip_to_md_window(frame);
    let _ = tx_by_ref(clipped);
}

fn tx_by_ref(frame: &mut [u8]) -> bool {
    if frame.is_empty() {
        return false;
    }
    // netstack_buf unused when callbacks are the no-op pair; null is fine.
    let rc = unsafe {
        esp_wifi_internal_tx_by_ref(
            WIFI_IF_STA,
            frame.as_mut_ptr().cast(),
            frame.len(),
            core::ptr::null_mut(),
        )
    };
    rc == ESP_OK
}

unsafe extern "C" fn netstack_ref(_buf: *mut c_void) {}
unsafe extern "C" fn netstack_free(_buf: *mut c_void) {}
