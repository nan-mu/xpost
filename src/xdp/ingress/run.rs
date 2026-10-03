//! Shared XDP `Prog::run` body for attach targets (bare Ingress or [`crate::TRACE`]).

use esp_radio::wifi::Action;
use portable_atomic::Ordering;

use crate::xdp::{
    counter::inc_bad_md,
    ingress::{action::XdpAction, run},
    state::ACTIVE,
};

/// Leased-image dispatch used by every attached [`esp_radio::wifi::Prog`].
#[esp_hal::ram]
pub(crate) fn ingress_run<'a>(frame: &'a mut [u8]) -> (Action, &'a mut [u8]) {
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
        inc_bad_md();
        return (bad_md_action, frame);
    }

    (ok_action, &mut frame[..window.len])
}
