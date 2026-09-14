//! XDP RX ingress via esp-radio's native-XDP [`Prog`] surface.
//!
//! Attaches [`XDP_PROG`] before `wifi::new`. Buffer ownership stays in esp-radio.
//! Packet window: no adjust_head; adjust_tail shrink-only → prefix of `frame`.

use portable_atomic::{AtomicUsize, Ordering};

use super::action::XdpAction;
use super::run;
use crate::xdp::state::ACTIVE;
use ::esp_radio::wifi::{Action, Prog, attach_prog};

/// Process-wide XDP program mounted on esp-radio RX ingress.
pub static XDP_PROG: XdpProg = XdpProg {
    drop_cnt: AtomicUsize::new(0),
    failed_tx_cnt: AtomicUsize::new(0),
};

/// Prog that invokes the leased XDP image and records driver counter callbacks.
pub struct XdpProg {
    drop_cnt: AtomicUsize,
    failed_tx_cnt: AtomicUsize,
}

impl XdpProg {
    #[inline]
    pub fn drop_count(&self) -> usize {
        self.drop_cnt.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn failed_tx_count(&self) -> usize {
        self.failed_tx_cnt.load(Ordering::Relaxed)
    }
}

impl Prog for XdpProg {
    fn run<'a>(&self, frame: &'a mut [u8]) -> (Action, &'a mut [u8]) {
        // Fast path: not leased → Pass without md / critical section.
        if !ACTIVE.load(Ordering::Acquire) {
            return (Action::Pass, frame);
        }
        let Some(result) = run(frame) else {
            return (Action::Pass, frame);
        };
        let (ok_action, bad_md_action) = match XdpAction::from(result.action) {
            XdpAction::Aborted | XdpAction::Drop => {
                return (Action::Drop, frame);
            }
            XdpAction::Pass => (Action::Pass, Action::Pass),
            XdpAction::Tx => (Action::Tx, Action::Drop),
        };

        let window = result.window;
        let valid = (window.data.cast_const() == frame.as_ptr()) & (window.len <= frame.len());

        if !valid {
            return (bad_md_action, frame);
        }

        (ok_action, &mut frame[..window.len])
    }

    #[inline]
    fn count_drop(&self) {
        self.drop_cnt.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    fn count_failed_tx(&self) {
        self.failed_tx_cnt.fetch_add(1, Ordering::Relaxed);
    }
}

/// Install the ingress prog before creating the stock Wi-Fi controller.
pub fn new<'d>(
    device: esp_hal::peripherals::WIFI<'d>,
    config: ::esp_radio::wifi::ControllerConfig,
) -> Result<
    (
        ::esp_radio::wifi::WifiController<'d>,
        ::esp_radio::wifi::Interfaces<'d>,
    ),
    ::esp_radio::wifi::WifiError,
> {
    // One-shot: first attach wins; retry `wifi::new` without re-attach.
    let _ = attach_prog(&XDP_PROG);
    ::esp_radio::wifi::new(device, config)
}
