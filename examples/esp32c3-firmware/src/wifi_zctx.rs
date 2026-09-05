//! Experimental XDP_TX zero-copy on ESP32-C3 Wi-Fi.
//!
//! Steal RX `eb` via `--wrap` of `esp_wifi_internal_free_rx_buffer`, then
//! `esp_wifi_internal_tx_by_ref`. lwIP-style refcount: we start at 1, blob
//! `ref_cb` may increment, **we drop our ref when `tx_by_ref` returns**.
//! If the blob never calls `free_cb`, that drop is what returns the DMA buffer
//! (otherwise `outstanding` sticks at 4 and the RX pool leaks).
//!
//! C3 is RV32IMC (no A extension): no `Atomic*::swap`.

use core::cell::RefCell;
use core::ffi::c_void;
use core::ptr;

use critical_section::Mutex;
use log::{error, info, warn};
use xpost::xdp::ZcTx;
use xpost::redirect::queue_egress_frame;

const WIFI_IF_STA: u32 = 0;
const ESP_OK: i32 = 0;
const MAX_OUTSTANDING: usize = 4;
/// Full-MTU (1492) `tx_by_ref` OOMs the leftover ~10KB Wi-Fi heap (alloc 192).
/// Echo-sized bounce is L2=76; keep zero-copy for that only.
const ZC_MAX_LEN: usize = 128;

#[derive(Clone, Copy)]
struct Hold {
    used: bool,
    refs: u8,
    eb: *mut c_void,
}

struct ZcState {
    want_steal: bool,
    stolen: bool,
    payload: *mut u8,
    len: usize,
    eb: *mut c_void,
    tag: u32,
    outstanding: usize,
    cb_ready: bool,
    same_ok: u32,
    same_fail: u32,
    free_cb_hits: u32,
    nomem: u32,
    holds: [Hold; MAX_OUTSTANDING],
}

unsafe impl Send for ZcState {}

const fn empty_holds() -> [Hold; MAX_OUTSTANDING] {
    [Hold {
        used: false,
        refs: 0,
        eb: ptr::null_mut(),
    }; MAX_OUTSTANDING]
}

static STATE: Mutex<RefCell<ZcState>> = Mutex::new(RefCell::new(ZcState {
    want_steal: false,
    stolen: false,
    payload: ptr::null_mut(),
    len: 0,
    eb: ptr::null_mut(),
    tag: 0,
    outstanding: 0,
    cb_ready: false,
    same_ok: 0,
    same_fail: 0,
    free_cb_hits: 0,
    nomem: 0,
    holds: empty_holds(),
}));

unsafe extern "C" {
    fn __real_esp_wifi_internal_free_rx_buffer(buffer: *mut c_void);
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

fn hold_index(p: *mut c_void) -> Option<usize> {
    if p.is_null() {
        return None;
    }
    critical_section::with(|cs| {
        let s = STATE.borrow_ref(cs);
        let base = s.holds.as_ptr() as usize;
        let addr = p as usize;
        if addr < base {
            return None;
        }
        let off = addr - base;
        if off % core::mem::size_of::<Hold>() != 0 {
            return None;
        }
        let i = off / core::mem::size_of::<Hold>();
        (i < MAX_OUTSTANDING).then_some(i)
    })
}

/// Drop one ref. When it hits 0, return the `eb` to actually free (outside CS).
fn drop_ref(netstack: *mut c_void) -> *mut c_void {
    let Some(i) = hold_index(netstack) else {
        return ptr::null_mut();
    };
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let h = &mut s.holds[i];
        if !h.used || h.refs == 0 {
            return ptr::null_mut();
        }
        h.refs -= 1;
        if h.refs != 0 {
            return ptr::null_mut();
        }
        let eb = h.eb;
        h.used = false;
        h.eb = ptr::null_mut();
        s.outstanding = s.outstanding.saturating_sub(1);
        eb
    })
}

fn alloc_hold(eb: *mut c_void) -> *mut c_void {
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        for h in s.holds.iter_mut() {
            if !h.used {
                h.used = true;
                h.refs = 1;
                h.eb = eb;
                return (h as *mut Hold).cast();
            }
        }
        ptr::null_mut()
    })
}

unsafe extern "C" fn netstack_ref(buf: *mut c_void) {
    let Some(i) = hold_index(buf) else {
        return;
    };
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let h = &mut s.holds[i];
        if h.used && h.refs < 8 {
            h.refs += 1;
        }
    });
}

unsafe extern "C" fn netstack_free(buf: *mut c_void) {
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        s.free_cb_hits = s.free_cb_hits.saturating_add(1);
    });
    let eb = drop_ref(buf);
    if !eb.is_null() {
        unsafe { __real_esp_wifi_internal_free_rx_buffer(eb) };
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_esp_wifi_internal_free_rx_buffer(buffer: *mut c_void) {
    let steal = critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        if s.want_steal {
            s.want_steal = false;
            s.eb = buffer;
            s.stolen = true;
            s.outstanding = s.outstanding.saturating_add(1);
            true
        } else {
            false
        }
    });
    if steal {
        return;
    }
    unsafe { __real_esp_wifi_internal_free_rx_buffer(buffer) };
}

unsafe fn load_tag(payload: *mut u8) -> u32 {
    unsafe { ptr::read_volatile(payload.cast::<u32>()) }
}

fn reclaim_leaked() {
    let mut ebs = [ptr::null_mut::<c_void>(); MAX_OUTSTANDING];
    let n = critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        let mut n = 0usize;
        for h in s.holds.iter_mut() {
            if h.used && !h.eb.is_null() {
                ebs[n] = h.eb;
                n += 1;
                h.used = false;
                h.refs = 0;
                h.eb = ptr::null_mut();
            }
        }
        let leaked = s.outstanding;
        s.outstanding = 0;
        if leaked > 0 {
            warn!("[zctx] reclaim leaked outstanding={leaked} (blob never free_cb?)");
        }
        n
    });
    for eb in ebs.iter().take(n) {
        unsafe { __real_esp_wifi_internal_free_rx_buffer(*eb) };
    }
}

fn arm(payload: *mut u8, len: usize) -> bool {
    if payload.is_null() || len < 4 {
        return false;
    }
    let need_reclaim =
        critical_section::with(|cs| STATE.borrow_ref(cs).outstanding >= MAX_OUTSTANDING);
    if need_reclaim {
        reclaim_leaked();
    }
    let tag = unsafe { load_tag(payload) };
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        if !s.cb_ready || len == 0 || len > ZC_MAX_LEN {
            return false;
        }
        if s.outstanding >= MAX_OUTSTANDING || s.want_steal || s.stolen {
            return false;
        }
        s.payload = payload;
        s.len = len;
        s.tag = tag;
        s.want_steal = true;
        true
    })
}

fn finish() {
    let (stolen, eb, payload, len, tag, outstanding) = critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        s.want_steal = false;
        let stolen = s.stolen;
        s.stolen = false;
        let eb = s.eb;
        let payload = s.payload;
        let len = s.len;
        let tag = s.tag;
        let outstanding = s.outstanding;
        s.eb = ptr::null_mut();
        s.payload = ptr::null_mut();
        s.len = 0;
        s.tag = 0;
        (stolen, eb, payload, len, tag, outstanding)
    });
    if !stolen && eb.is_null() && payload.is_null() {
        return;
    }
    if !stolen {
        error!("[zctx] steal missed (linker --wrap inactive?); drop XDP_TX");
        return;
    }
    if eb.is_null() || payload.is_null() || len == 0 {
        error!("[zctx] stolen frame missing ptr");
        if !eb.is_null() {
            unsafe { __real_esp_wifi_internal_free_rx_buffer(eb) };
            critical_section::with(|cs| {
                let mut s = STATE.borrow_ref_mut(cs);
                s.outstanding = s.outstanding.saturating_sub(1);
            });
        }
        return;
    }
    let live = unsafe { load_tag(payload) };
    let same = live == tag;
    critical_section::with(|cs| {
        let mut s = STATE.borrow_ref_mut(cs);
        if same {
            s.same_ok = s.same_ok.saturating_add(1);
        } else {
            s.same_fail = s.same_fail.saturating_add(1);
        }
    });
    // Shrunk XDP_TX is the zero-copy proof (firewall_array adjust_tail). Full
    // MTU is still 1492; do not treat a shorter window as an error.
    let hold = alloc_hold(eb);
    if hold.is_null() {
        error!("[zctx] no hold slot, free eb");
        unsafe { __real_esp_wifi_internal_free_rx_buffer(eb) };
        critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            s.outstanding = s.outstanding.saturating_sub(1);
        });
        return;
    }
    let logged = critical_section::with(|cs| {
        let s = STATE.borrow_ref(cs);
        s.same_ok.saturating_add(s.same_fail)
    });
    // Per-TX `info!` stalls USB-UART (~10ms/line) and overflows rx_queue=16.
    if logged <= 1 || !same {
        info!("[zctx] zc len={len} tag_ok={same} outstanding={outstanding}");
    }
    let rc = unsafe { esp_wifi_internal_tx_by_ref(WIFI_IF_STA, payload.cast(), len, hold) };
    if rc != ESP_OK {
        let n = critical_section::with(|cs| {
            let mut s = STATE.borrow_ref_mut(cs);
            s.nomem = s.nomem.saturating_add(1);
            s.nomem
        });
        if rc != 257 || n == 1 || n.is_multiple_of(256) {
            warn!("[zctx] tx_by_ref err={rc} nomem={n}");
        }
        // Copy while `payload` is still inside the stolen RX buffer.
        let frame = unsafe { core::slice::from_raw_parts(payload, len) };
        let _ = queue_egress_frame(frame);
    }
    // Always drop *our* ref (lwIP pbuf_free after tx_by_ref). If the blob never
    // calls free_cb, this is what prevents a permanent RX-pool leak.
    let leftover = critical_section::with(|cs| {
        let Some(i) = hold_index(hold) else {
            return 0u8;
        };
        STATE.borrow_ref(cs).holds[i].refs
    });
    let eb_now = drop_ref(hold);
    if !eb_now.is_null() {
        unsafe { __real_esp_wifi_internal_free_rx_buffer(eb_now) };
    } else if leftover > 1 {
        log::debug!("[zctx] wifi holds leftover={leftover} after tx_by_ref");
    }
    if rc != ESP_OK {
        let extra = drop_ref(hold);
        if !extra.is_null() {
            unsafe { __real_esp_wifi_internal_free_rx_buffer(extra) };
        }
    }
}

/// `arm` failed: still copy (small or large). Dropping here made 0µs Xloss 100%.
fn copy_ok(_len: usize) -> bool {
    true
}

pub fn init() {
    let rc =
        unsafe { esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free)) };
    if rc != ESP_OK {
        error!("[zctx] reg_netstack_buf_cb failed rc={rc}");
        critical_section::with(|cs| {
            STATE.borrow_ref_mut(cs).cb_ready = false;
        });
        return;
    }
    critical_section::with(|cs| {
        STATE.borrow_ref_mut(cs).cb_ready = true;
    });
    crate::init_log!("[zctx] tx_by_ref armed (RX steal via --wrap free_rx_buffer)");
}

pub const HOOKS: ZcTx = ZcTx {
    arm,
    finish,
    copy_ok,
};
