#![allow(unsafe_op_in_unsafe_fn)]
//! XDP packet context (`xdp_md` ABI).
//!
//! Layout matches Linux `struct xdp_md` field offsets on RV32 (`ilp32`):
//! `data`/`data_end` at 0/4, `data_meta` at 8, `ingress_ifindex` at 0xc.
//!
//! First-case programs mutate the packet via direct pointer access.
//! `bpf_xdp_adjust_tail` (shrink-only) may move `data_end`; load_bytes /
//! store_bytes / adjust_head remain unsupported.

/// Compile-time logical port table (stable across SoCs; not Linux ifindex).
pub(crate) const IFINDEX_WIFI: u32 = 0;
/// Wired NIC — reserved stub on ESP32-C3.
#[allow(dead_code)]
pub(crate) const IFINDEX_WIRED: u32 = 1;

#[repr(C)]
pub(crate) struct XdpMd {
    pub data: *mut u8,
    pub data_end: *mut u8,
    pub data_meta: u32,
    pub ingress_ifindex: u32,
    pub rx_queue_index: u32,
    pub egress_ifindex: u32,
}

impl XdpMd {
    /// Build a wifi-ingress context over `buf`.
    #[inline]
    pub fn from_buf(buf: &mut [u8]) -> Self {
        Self {
            data: buf.as_mut_ptr(),
            data_end: unsafe { buf.as_mut_ptr().add(buf.len()) },
            data_meta: 0,
            ingress_ifindex: IFINDEX_WIFI,
            rx_queue_index: 0,
            egress_ifindex: 0,
        }
    }
}
