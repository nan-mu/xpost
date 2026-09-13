//! XDP action codes (`enum xdp_action`).

use core::fmt;

/// Linux `enum xdp_action` as returned by the XDP image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum XdpAction {
    Aborted = 0,
    Drop = 1,
    Pass = 2,
    Tx = 3,
    /// ABI value retained; redirect helper is unsupported (treated as drop).
    Redirect = 4,
}

impl From<i32> for XdpAction {
    #[inline]
    fn from(v: i32) -> Self {
        match v {
            0 => Self::Aborted,
            1 => Self::Drop,
            2 => Self::Pass,
            3 => Self::Tx,
            4 => Self::Redirect,
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
            Self::Redirect => "XDP_REDIRECT",
        })
    }
}
