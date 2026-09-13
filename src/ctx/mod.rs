#![allow(unsafe_op_in_unsafe_fn)]
//! XDP packet context (`xdp_md` ABI).
//!
//! Layout matches Linux `struct xdp_md` field offsets on RV32 (`ilp32`):
//! `data`/`data_end` at 0/4, `data_meta` at 8, `ingress_ifindex` at 0xc.
//!
//! First-case programs mutate the packet via direct pointer access.
//! `bpf_xdp_adjust_tail` (shrink-only) may move `data_end`; load_bytes /
//! store_bytes / adjust_head remain unsupported.

use core::cell::RefCell;

use critical_section::Mutex;

/// Compile-time logical port table (stable across SoCs; not Linux ifindex).
pub(crate) const IFINDEX_WIFI: u32 = 0;
/// Wired NIC — reserved stub on ESP32-C3.
#[allow(dead_code)]
pub(crate) const IFINDEX_WIRED: u32 = 1;

#[repr(C)]
pub struct XdpMd {
    pub data: *mut u8,
    pub data_end: *mut u8,
    pub data_meta: u32,
    pub ingress_ifindex: u32,
    pub rx_queue_index: u32,
    pub egress_ifindex: u32,
}

/// Alias used by AOT load path.
#[allow(dead_code)] // used from ingress flight path
pub(crate) type XdpContext = XdpMd;

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

/// Last AOT `data`/`data_end` window (pointer as usize). `(0, 0)` = none.
#[allow(dead_code)] // written/read by window helpers when radio path is linked
static LAST_WINDOW: Mutex<RefCell<(usize, usize)>> = Mutex::new(RefCell::new((0, 0)));

/// Snapshot `data`/`data_end` after invoke so XDP_TX can clip before `tx_by_ref`.
#[allow(dead_code)] // used from ingress / esp_radio when those features are on
pub(crate) fn record_window(md: &XdpMd) {
    let ptr = md.data as usize;
    let end = md.data_end as usize;
    let len = end.saturating_sub(ptr);
    critical_section::with(|cs| {
        *LAST_WINDOW.borrow_ref_mut(cs) = (ptr, len);
    });
}

#[allow(dead_code)] // paired with record_window
pub(crate) fn clear_window() {
    critical_section::with(|cs| {
        *LAST_WINDOW.borrow_ref_mut(cs) = (0, 0);
    });
}

/// Clip `buffer` to the last recorded md window if it lies inside `buffer`.
#[allow(dead_code)] // used from ingress::esp_radio (XDP_TX clip)
pub(crate) fn clip_to_md_window(buffer: &mut [u8]) -> &mut [u8] {
    let (ptr, len) = critical_section::with(|cs| {
        core::mem::replace(&mut *LAST_WINDOW.borrow_ref_mut(cs), (0, 0))
    });
    if ptr == 0 || len == 0 {
        return buffer;
    }
    let start = buffer.as_mut_ptr() as usize;
    if ptr < start {
        return buffer;
    }
    let off = ptr - start;
    let Some(end) = off.checked_add(len) else {
        return buffer;
    };
    if end > buffer.len() {
        return buffer;
    }
    &mut buffer[off..end]
}
