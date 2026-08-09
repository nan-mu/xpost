#![allow(unsafe_op_in_unsafe_fn)]
//! XDP packet context (`xdp_md` ABI).
//!
//! Layout matches Linux `struct xdp_md` field offsets on RV32 (`ilp32`):
//! `data`/`data_end` at 0/4, `data_meta` at 8, `ingress_ifindex` at 0xc.

use core::ptr;

/// WiFi ingress ifindex (compile-time port table: 0 = wifi).
pub const IFINDEX_WIFI: u32 = 0;

#[repr(C)]
pub struct XdpMd {
    pub data: *mut u8,
    pub data_end: *mut u8,
    pub data_meta: u32,
    pub ingress_ifindex: u32,
    pub rx_queue_index: u32,
    pub egress_ifindex: u32,
}

/// Alias used by AOT load path (`part`).
pub type XdpContext = XdpMd;

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

    #[inline]
    pub unsafe fn from_raw(xdp_md: usize) -> Option<&'static mut Self> {
        if xdp_md == 0 {
            return None;
        }
        Some(&mut *(xdp_md as *mut Self))
    }

    /// `bpf_xdp_adjust_head`: positive delta shrinks from the front.
    pub unsafe fn adjust_head(&mut self, delta: usize) -> i32 {
        let delta = delta as i32;
        if delta < 0 {
            return -22;
        }
        let d = delta as usize;
        let new_data = match (self.data as usize).checked_add(d) {
            Some(v) => v,
            None => return -22,
        };
        if new_data > self.data_end as usize {
            return -22;
        }
        self.data = new_data as *mut u8;
        0
    }

    /// `bpf_xdp_adjust_tail`: negative delta shrinks; positive grow rejected.
    pub unsafe fn adjust_tail(&mut self, delta: usize) -> i32 {
        let delta = delta as i32;
        if delta > 0 {
            return -22;
        }
        let new_end = (self.data_end as isize).wrapping_add(delta as isize);
        if new_end < self.data as isize {
            return -22;
        }
        self.data_end = new_end as *mut u8;
        0
    }

    pub unsafe fn load_bytes(&self, offset: usize, buf: usize, len: usize) -> i32 {
        if buf == 0 || len == 0 {
            return -22;
        }
        let start = match (self.data as usize).checked_add(offset) {
            Some(v) => v,
            None => return -22,
        };
        let end = match start.checked_add(len) {
            Some(v) => v,
            None => return -22,
        };
        if end > self.data_end as usize {
            return -22;
        }
        ptr::copy_nonoverlapping(start as *const u8, buf as *mut u8, len);
        0
    }

    pub unsafe fn store_bytes(&self, offset: usize, buf: usize, len: usize) -> i32 {
        if buf == 0 || len == 0 {
            return -22;
        }
        let start = match (self.data as usize).checked_add(offset) {
            Some(v) => v,
            None => return -22,
        };
        let end = match start.checked_add(len) {
            Some(v) => v,
            None => return -22,
        };
        if end > self.data_end as usize {
            return -22;
        }
        ptr::copy_nonoverlapping(buf as *const u8, start as *mut u8, len);
        0
    }
}
