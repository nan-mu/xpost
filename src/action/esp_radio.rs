//! Apply an `xdp_action` to an esp-radio RX buffer (outside the AOT CS).

use core::ffi::c_void;

use crate::action::{XDP_ABORTED, XDP_DROP, XDP_PASS, XDP_TX, queue_egress_frame};
use crate::xdp::ingress::esp_radio::{ESP_OK, call_orig, free_rx_buffer, tx_by_ref};

/// Consume `action` for one RX delivery. Must not run under the AOT critical section.
pub(crate) unsafe fn apply(
    ifx: u32,
    buffer: *mut c_void,
    len: u16,
    eb: *mut c_void,
    frame: &mut [u8],
    action: i32,
) -> i32 {
    match action {
        XDP_PASS => unsafe { call_orig(ifx, buffer, len, eb) },
        XDP_DROP | XDP_ABORTED => {
            finish_local(eb);
            ESP_OK
        }
        XDP_TX => {
            let clipped = crate::ctx::clip_to_md_window(frame);
            if !tx_by_ref(clipped) {
                let _ = queue_egress_frame(clipped);
            }
            finish_local(eb);
            ESP_OK
        }
        _ => {
            // Unsupported actions (e.g. XDP_REDIRECT without helper): drop.
            finish_local(eb);
            ESP_OK
        }
    }
}

#[inline]
fn finish_local(eb: *mut c_void) {
    if !eb.is_null() {
        free_rx_buffer(eb);
    }
}
