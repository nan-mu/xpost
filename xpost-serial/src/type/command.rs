//! Wire / host request shapes. `no_std`, no `alloc`.
//!
//! Parsing of UART request lines lives on the server; host formatting of
//! **wire** variants lives on the client. [`Command::GetInit`] is host-only
//! (never serialized).

use core::time::Duration;

/// Host wait for unsolicited [`crate::reply::OkReply::Init`] when using
/// [`Command::GetInit`]. Not a UART deadline for a request line.
pub const GET_INIT_TIMEOUT: Duration = Duration::from_secs(30);

/// App-mode command shape.
///
/// Most variants are UART request lines. [`GetInit`](Command::GetInit) is
/// **host-only**: it must never be written to the serial port; the host only
/// scans / waits on the transcript for `OK init` (deadline
/// [`GET_INIT_TIMEOUT`]).
pub enum Command<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap {
        name: &'a str,
    },
    GetMap {
        name: &'a str,
    },
    /// Names borrow ASCII; key/value borrow the original wire tokens, not decoded
    /// integers. The firmware validates them against the selected map's schema.
    UpdateMap {
        name: &'a str,
        key: &'a [u8],
        value: &'a [u8],
    },
    ClearPerf,
    GetPerf,
    /// Wait for unsolicited `OK init` in the host record. **Not a wire verb** —
    /// must not be sent on UART; server must not parse a request into this.
    GetInit,
}
