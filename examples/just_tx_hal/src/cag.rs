//! Local CAG wire report (counter+aggregate+gauge = 1624 bytes), no xpost.

use core::cell::RefCell;
use core::convert::Infallible;

use critical_section::Mutex;

/// CAG payload: 184 + 8*176 + 32.
pub const CAG_WIRE_LEN: usize = 184 + 1408 + 32;

static REPORT: Mutex<RefCell<[u8; CAG_WIRE_LEN]>> =
    Mutex::new(RefCell::new([0u8; CAG_WIRE_LEN]));

/// Reset the local CAG buffer to zeros.
pub fn clear_perf() {
    critical_section::with(|cs| {
        REPORT.borrow(cs).borrow_mut().fill(0);
    });
}

/// Payload length for the compiled CAG layout.
pub const fn report_payload_len() -> usize {
    CAG_WIRE_LEN
}

/// Stream the CAG report through `write` (raw LE bytes, no framing).
pub fn report_perf(mut write: impl FnMut(&[u8]) -> Result<(), Infallible>) {
    critical_section::with(|cs| {
        let buf = REPORT.borrow(cs).borrow();
        let _ = write(&buf[..]);
    });
}
