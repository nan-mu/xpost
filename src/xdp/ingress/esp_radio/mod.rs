//! Replace `esp_radio::wifi::new` so XDP runs before the radio RX queue.
//!
//! Internals capture the original `esp_wifi_internal_reg_rxcb` callbacks via
//! linker `--wrap` and install a trampoline. Program run is [`super::run`];
//! action handling is [`crate::action`].

use core::ffi::c_void;
use core::sync::atomic::Ordering;

use portable_atomic::{AtomicPtr, AtomicUsize};

type WifiRxCb = Option<unsafe extern "C" fn(*mut c_void, u16, *mut c_void) -> i32>;

static ORIG_STA: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static ORIG_AP: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static WRAP_READY: AtomicUsize = AtomicUsize::new(0);

pub(crate) const WIFI_IF_STA: u32 = 0;
pub(crate) const ESP_OK: i32 = 0;
const ESP_IF_WIFI_STA: u32 = 0;
const ESP_IF_WIFI_AP: u32 = 1;

unsafe extern "C" {
    fn __real_esp_wifi_internal_reg_rxcb(ifx: u32, fn_: WifiRxCb) -> i32;
    fn esp_wifi_internal_free_rx_buffer(buffer: *mut c_void);
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

/// Prepare the XDP trampoline, then call the stock `esp_radio::wifi::new`.
pub fn new<'d>(
    device: esp_hal::peripherals::WIFI<'d>,
    config: esp_radio::wifi::ControllerConfig,
) -> Result<
    (
        esp_radio::wifi::WifiController<'d>,
        esp_radio::wifi::Interfaces<'d>,
    ),
    esp_radio::wifi::WifiError,
> {
    arm_netstack_callbacks();
    WRAP_READY.store(1, Ordering::Release);
    esp_radio::wifi::new(device, config)
}

fn arm_netstack_callbacks() {
    let rc =
        unsafe { esp_wifi_internal_reg_netstack_buf_cb(Some(netstack_ref), Some(netstack_free)) };
    if rc != ESP_OK {
        // TX zero-copy may degrade to copy fallback; still allow PASS/DROP.
        log::warn!("[xdp][esp-radio] reg_netstack_buf_cb rc={rc}");
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_esp_wifi_internal_reg_rxcb(ifx: u32, fn_: WifiRxCb) -> i32 {
    let ptr = fn_.map(|f| f as *mut ()).unwrap_or(core::ptr::null_mut());
    match ifx {
        ESP_IF_WIFI_STA => {
            ORIG_STA.store(ptr, Ordering::Release);
            unsafe { __real_esp_wifi_internal_reg_rxcb(ifx, Some(trampoline_sta)) }
        }
        ESP_IF_WIFI_AP => {
            ORIG_AP.store(ptr, Ordering::Release);
            unsafe { __real_esp_wifi_internal_reg_rxcb(ifx, Some(trampoline_ap)) }
        }
        _ => unsafe { __real_esp_wifi_internal_reg_rxcb(ifx, fn_) },
    }
}

unsafe extern "C" fn trampoline_sta(buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    unsafe { hook(ESP_IF_WIFI_STA, buffer, len, eb) }
}

unsafe extern "C" fn trampoline_ap(buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    unsafe { hook(ESP_IF_WIFI_AP, buffer, len, eb) }
}

/// RX hook: run XDP when leased, then apply the action outside the AOT CS.
unsafe fn hook(ifx: u32, buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    if WRAP_READY.load(Ordering::Acquire) == 0 || buffer.is_null() || len == 0 {
        return unsafe { call_orig(ifx, buffer, len, eb) };
    }
    let frame = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), len as usize) };
    let Some(action) = super::run(frame) else {
        return unsafe { call_orig(ifx, buffer, len, eb) };
    };
    unsafe { crate::action::apply(ifx, buffer, len, eb, frame, action) }
}

pub(crate) unsafe fn call_orig(ifx: u32, buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    let ptr = match ifx {
        ESP_IF_WIFI_STA => ORIG_STA.load(Ordering::Acquire),
        ESP_IF_WIFI_AP => ORIG_AP.load(Ordering::Acquire),
        _ => core::ptr::null_mut(),
    };
    if ptr.is_null() {
        if !eb.is_null() {
            free_rx_buffer(eb);
        }
        return ESP_OK;
    }
    let cb: unsafe extern "C" fn(*mut c_void, u16, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(ptr) };
    unsafe { cb(buffer, len, eb) }
}

#[inline]
pub(crate) fn free_rx_buffer(buffer: *mut c_void) {
    unsafe { esp_wifi_internal_free_rx_buffer(buffer) };
}

pub(crate) fn tx_by_ref(frame: &mut [u8]) -> bool {
    if frame.is_empty() {
        return false;
    }
    // netstack_buf is unused by the blob when callbacks are registered with a
    // simple no-op pair; pass null and let free_rx_buffer reclaim on miss.
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
