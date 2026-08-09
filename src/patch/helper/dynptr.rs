#![allow(unsafe_op_in_unsafe_fn)]
//! MCU `bpf_dynptr` — RV32 16-byte layout (`struct bpf_dynptr { __u64 __opaque[2]; }`).
//!
//! # Contiguous objects first
//!
//! Primary model: a **contiguous byte span** `(base, len)` — Linux
//! `BPF_DYNPTR_TYPE_LOCAL`. [`BpfDynptr::from_contig`] / [`BpfDynptr::from_mem`]
//! wrap that span; read / write / data operate only within it. Ringbuf dynptrs
//! (`TYPE_RINGBUF`) are a separate path built on the same header.
//!
//! Layout (LE RV32, 8-byte aligned):
//! - `data: u32` — absolute base pointer
//! - `size: u32` — bits 0..23 usable length; 24..30 type; bit 31 read-only
//! - `offset: u32` — base offset into `data`
//! - `_pad: u32`

use core::ptr;

/// Matches `enum bpf_dynptr_type` (Linux).
pub const TYPE_INVALID: u32 = 0;
/// Contiguous local object (primary MCU path).
pub const TYPE_LOCAL: u32 = 1;
pub const TYPE_RINGBUF: u32 = 2;

const SIZE_MASK: u32 = 0x00ff_ffff;
const TYPE_SHIFT: u32 = 24;
const RDONLY_BIT: u32 = 1 << 31;

/// A contiguous byte object: dynptr LOCAL views this span and nothing else.
#[derive(Clone, Copy)]
#[allow(dead_code)] // used by `hw-selftest` / userspace contig helpers
pub struct ContigObj {
    pub base: usize,
    pub len: usize,
}

impl ContigObj {
    #[allow(dead_code)]
    pub fn from_slice(s: &mut [u8]) -> Self {
        Self {
            base: s.as_mut_ptr() as usize,
            len: s.len(),
        }
    }
}

#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct BpfDynptr {
    pub data: u32,
    pub size: u32,
    pub offset: u32,
    pub _pad: u32,
}

impl BpfDynptr {
    pub const fn null() -> Self {
        Self {
            data: 0,
            size: 0,
            offset: 0,
            _pad: 0,
        }
    }

    pub fn new(data: usize, usable: u32, ty: u32, rdonly: bool) -> Self {
        Self {
            data: data as u32,
            size: Self::pack_size(usable, ty, rdonly),
            offset: 0,
            _pad: 0,
        }
    }

    pub fn usable_size(self) -> u32 {
        self.size & SIZE_MASK
    }

    pub fn dynptr_type(self) -> u32 {
        (self.size >> TYPE_SHIFT) & 0x7f
    }

    pub fn is_rdonly(self) -> bool {
        self.size & RDONLY_BIT != 0
    }

    pub fn is_valid(self) -> bool {
        self.data != 0 && self.dynptr_type() != TYPE_INVALID && self.usable_size() > 0
    }

    #[allow(dead_code)]
    pub fn is_contig(self) -> bool {
        self.is_valid() && self.dynptr_type() == TYPE_LOCAL
    }

    /// Absolute address of the first usable byte (`data + offset`).
    pub fn base_ptr(self) -> usize {
        (self.data as usize).wrapping_add(self.offset as usize)
    }

    fn pack_size(usable: u32, ty: u32, rdonly: bool) -> u32 {
        (usable & SIZE_MASK) | ((ty & 0x7f) << TYPE_SHIFT) | if rdonly { RDONLY_BIT } else { 0 }
    }

    unsafe fn read_at(p: usize) -> Option<Self> {
        if p == 0 {
            return None;
        }
        Some(ptr::read_unaligned(p as *const Self))
    }

    unsafe fn write_at(p: usize, v: Self) {
        if p != 0 {
            ptr::write_unaligned(p as *mut Self, v);
        }
    }

    /// Wrap a contiguous object as `TYPE_LOCAL` dynptr (preferred entry).
    #[allow(dead_code)]
    pub fn from_contig(obj: ContigObj, out: usize) -> usize {
        Self::from_mem(obj.base, obj.len, 0, out)
    }

    /// `bpf_dynptr_from_mem(data, size, flags, ptr)` → 0 / negated errno.
    ///
    /// Semantics: `data` must address a contiguous `size`-byte object.
    pub fn from_mem(data: usize, size: usize, flags: usize, ptr: usize) -> usize {
        if flags != 0 || ptr == 0 || data == 0 || size == 0 {
            return (-22i32) as usize;
        }
        if size > SIZE_MASK as usize {
            return (-7i32) as usize; // E2BIG
        }
        unsafe {
            Self::write_at(ptr, Self::new(data, size as u32, TYPE_LOCAL, false));
        }
        0
    }

    /// `bpf_dynptr_read(dst, len, src, offset, flags)`.
    pub fn read(dst: usize, len: usize, src: usize, offset: usize, flags: usize) -> usize {
        if flags != 0 || dst == 0 || len == 0 {
            return (-22i32) as usize;
        }
        let Some(dp) = (unsafe { Self::read_at(src) }) else {
            return (-22i32) as usize;
        };
        if !dp.is_valid() {
            return (-22i32) as usize;
        }
        let usable = dp.usable_size() as usize;
        if offset.checked_add(len).map(|e| e > usable).unwrap_or(true) {
            return (-7i32) as usize; // E2BIG
        }
        let src_addr = dp.base_ptr().wrapping_add(offset);
        unsafe {
            ptr::copy_nonoverlapping(src_addr as *const u8, dst as *mut u8, len);
        }
        0
    }

    /// `bpf_dynptr_write(dst, offset, src, len, flags)`.
    pub fn write(dst: usize, offset: usize, src: usize, len: usize, flags: usize) -> usize {
        if flags != 0 || src == 0 || len == 0 {
            return (-22i32) as usize;
        }
        let Some(dp) = (unsafe { Self::read_at(dst) }) else {
            return (-22i32) as usize;
        };
        if !dp.is_valid() {
            return (-22i32) as usize;
        }
        if dp.is_rdonly() {
            return (-22i32) as usize;
        }
        let usable = dp.usable_size() as usize;
        if offset.checked_add(len).map(|e| e > usable).unwrap_or(true) {
            return (-7i32) as usize;
        }
        let dst_addr = dp.base_ptr().wrapping_add(offset);
        unsafe {
            ptr::copy_nonoverlapping(src as *const u8, dst_addr as *mut u8, len);
        }
        0
    }

    /// `bpf_dynptr_data(ptr, offset, len)` → pointer or 0 (NULL).
    pub fn data(ptr: usize, offset: usize, len: usize) -> usize {
        let Some(dp) = (unsafe { Self::read_at(ptr) }) else {
            return 0;
        };
        if !dp.is_valid() || len == 0 {
            return 0;
        }
        if dp.is_rdonly() {
            return 0;
        }
        let usable = dp.usable_size() as usize;
        if offset.checked_add(len).map(|e| e > usable).unwrap_or(true) {
            return 0;
        }
        dp.base_ptr().wrapping_add(offset)
    }

    /// Init dynptr over a ringbuf reserved payload (`bpf_ringbuf_reserve_dynptr`).
    pub fn set_ringbuf(ptr: usize, data: usize, size: usize) {
        if size == 0 || size > SIZE_MASK as usize {
            unsafe { Self::write_at(ptr, Self::null()) };
            return;
        }
        unsafe {
            Self::write_at(ptr, Self::new(data, size as u32, TYPE_RINGBUF, false));
        }
    }

    pub fn clear(ptr: usize) {
        unsafe { Self::write_at(ptr, Self::null()) };
    }

    /// Payload address for ringbuf submit/discard, if this is a RINGBUF dynptr.
    pub fn ringbuf_payload(ptr: usize) -> Option<usize> {
        let dp = unsafe { Self::read_at(ptr)? };
        if dp.dynptr_type() != TYPE_RINGBUF || dp.data == 0 {
            return None;
        }
        Some(dp.base_ptr())
    }
}
