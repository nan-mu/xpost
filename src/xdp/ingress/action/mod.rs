//! XDP action constants, TX egress queue, and return-value apply.

mod egress;
#[cfg(feature = "esp32c3-radio")]
mod esp_radio;

pub use egress::queue_egress_frame;

/// Linux `enum xdp_action` values used by the XDP image / helpers.
pub const XDP_ABORTED: i32 = 0;
pub const XDP_DROP: i32 = 1;
pub const XDP_PASS: i32 = 2;
pub const XDP_TX: i32 = 3;
/// ABI value retained; redirect helper is unsupported (treated as drop).
pub const XDP_REDIRECT: i32 = 4;

/// Name for driver / debug log lines (`enum xdp_action`).
#[inline]
pub fn xdp_action_name(action: i32) -> &'static str {
    match action {
        XDP_ABORTED => "XDP_ABORTED",
        XDP_DROP => "XDP_DROP",
        XDP_PASS => "XDP_PASS",
        XDP_TX => "XDP_TX",
        XDP_REDIRECT => "XDP_REDIRECT",
        _ => "XDP_UNKNOWN",
    }
}

#[cfg(feature = "esp32c3-radio")]
pub(crate) use esp_radio::apply;
