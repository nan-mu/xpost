//! Replace `esp_radio::wifi::new` so XDP runs before the radio RX queue.
//!
//! Internals capture the original `esp_wifi_internal_reg_rxcb` callbacks via
//! linker `--wrap` and install a trampoline. Program run is [`super::run`];
//! action apply stays here (outside the invoke CS); [`super::egress`] owns XDP_TX.

use core::ffi::c_void;
use core::sync::atomic::Ordering;

use portable_atomic::{AtomicPtr, AtomicUsize};

use super::action::XdpAction;
use crate::xdp::state::{ESP_OK, WIFI_IF_AP, WIFI_IF_STA};

type WifiRxCb = Option<unsafe extern "C" fn(*mut c_void, u16, *mut c_void) -> i32>;

static ORIG_STA: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static ORIG_AP: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static WRAP_READY: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" {
    fn __real_esp_wifi_internal_reg_rxcb(ifx: u32, fn_: WifiRxCb) -> i32;
    fn esp_wifi_internal_free_rx_buffer(buffer: *mut c_void);
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
    WRAP_READY.store(1, Ordering::Release);
    let radio = esp_radio::wifi::new(device, config);
    if radio.is_ok() {
        // esp-radio initializes the Wi-Fi driver (and RX callbacks) in `new`.
        super::egress::arm_zero_copy();
    }
    radio
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_esp_wifi_internal_reg_rxcb(ifx: u32, fn_: WifiRxCb) -> i32 {
    let ptr = fn_.map(|f| f as *mut ()).unwrap_or(core::ptr::null_mut());
    match ifx {
        WIFI_IF_STA => {
            ORIG_STA.store(ptr, Ordering::Release);
            unsafe { __real_esp_wifi_internal_reg_rxcb(ifx, Some(trampoline_sta)) }
        }
        WIFI_IF_AP => {
            ORIG_AP.store(ptr, Ordering::Release);
            unsafe { __real_esp_wifi_internal_reg_rxcb(ifx, Some(trampoline_ap)) }
        }
        _ => unsafe { __real_esp_wifi_internal_reg_rxcb(ifx, fn_) },
    }
}

unsafe extern "C" fn trampoline_sta(buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    unsafe { hook(WIFI_IF_STA, buffer, len, eb) }
}

unsafe extern "C" fn trampoline_ap(buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    unsafe { hook(WIFI_IF_AP, buffer, len, eb) }
}

/// RX hook: run XDP when leased, then apply the action outside the invoke CS.
unsafe fn hook(ifx: u32, buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    if WRAP_READY.load(Ordering::Acquire) == 0 || buffer.is_null() || len == 0 {
        return unsafe { call_orig(ifx, buffer, len, eb) };
    }
    let frame = unsafe { core::slice::from_raw_parts_mut(buffer.cast::<u8>(), len as usize) };
    let Some(result) = super::run(frame) else {
        return unsafe { call_orig(ifx, buffer, len, eb) };
    };
    unsafe { apply(ifx, buffer, len, eb, frame, result) }
}

/// Consume `action` for one RX delivery. Must not run under the invoke CS.
unsafe fn apply(
    ifx: u32,
    buffer: *mut c_void,
    len: u16,
    eb: *mut c_void,
    frame: &mut [u8],
    result: super::RunResult,
) -> i32 {
    match XdpAction::from(result.action) {
        XdpAction::Pass => unsafe { call_orig(ifx, buffer, len, eb) },
        XdpAction::Drop | XdpAction::Aborted => {
            finish_local(eb);
            ESP_OK
        }
        XdpAction::Tx => {
            // XDP_TX (and ABI XDP_REDIRECT rewritten to Tx): same RX iface.
            let _ = super::egress::transmit(ifx, frame, result.window, eb);
            ESP_OK
        }
    }
}

unsafe fn call_orig(ifx: u32, buffer: *mut c_void, len: u16, eb: *mut c_void) -> i32 {
    let ptr = match ifx {
        WIFI_IF_STA => ORIG_STA.load(Ordering::Acquire),
        WIFI_IF_AP => ORIG_AP.load(Ordering::Acquire),
        _ => core::ptr::null_mut(),
    };
    if ptr.is_null() {
        finish_local(eb);
        return ESP_OK;
    }
    let cb: unsafe extern "C" fn(*mut c_void, u16, *mut c_void) -> i32 =
        unsafe { core::mem::transmute(ptr) };
    unsafe { cb(buffer, len, eb) }
}

#[inline]
fn finish_local(eb: *mut c_void) {
    if !eb.is_null() {
        unsafe { esp_wifi_internal_free_rx_buffer(eb) };
    }
}
