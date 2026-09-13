//! XDP action codes (`enum xdp_action`).
//!
//! This target only implements TX egress. ABI value `XDP_REDIRECT` (4) is
//! rewritten to [`XdpAction::Tx`] at the apply boundary.

use core::fmt;

/// Linux `enum xdp_action` as returned by the XDP image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum XdpAction {
    Aborted = 0,
    Drop = 1,
    Pass = 2,
    Tx = 3,
}

impl From<i32> for XdpAction {
    #[inline]
    fn from(v: i32) -> Self {
        match v {
            0 => Self::Aborted,
            1 => Self::Drop,
            2 => Self::Pass,
            // XDP_TX (3) and unsupported XDP_REDIRECT (4) → same-iface TX.
            3 | 4 => Self::Tx,
            // Fail closed: unknown codes behave like abort at the apply site.
            _ => Self::Aborted,
        }
    }
}

impl fmt::Display for XdpAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Aborted => "XDP_ABORTED",
            Self::Drop => "XDP_DROP",
            Self::Pass => "XDP_PASS",
            Self::Tx => "XDP_TX",
        })
    }
}
