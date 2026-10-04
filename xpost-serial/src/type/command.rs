//! Wire / host request shapes. `no_std`, no `alloc`.
//!
//! Parsing of UART request lines lives on the server; host formatting of
//! **wire** variants lives on the client. [`Command::GetInit`] and
//! [`Command::GetFinish`] are host-only (never serialized).

use core::time::Duration;

/// Host wait for unsolicited [`crate::reply::OkReply::Init`] when using
/// [`Command::GetInit`]. Not a UART deadline for a request line.
pub const GET_INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Host wait for unsolicited [`crate::reply::OkReply::Finish`] when using
/// [`Command::GetFinish`]. Not a UART deadline for a request line.
pub const GET_FINISH_TIMEOUT: Duration = Duration::from_secs(30);

/// App-mode command shape.
///
/// Most variants are UART request lines. [`GetInit`](Command::GetInit) and
/// [`GetFinish`](Command::GetFinish) are **host-only**: they must never be
/// written to the serial port; the host only scans / waits on the transcript
/// (deadlines [`GET_INIT_TIMEOUT`] / [`GET_FINISH_TIMEOUT`]).
pub enum Command<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    EnableExec,
    DisableExec,
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
    /// Wait for unsolicited `OK finish` in the host record (e.g. eBPF
    /// `bpf_printk`). **Not a wire verb** — must not be sent on UART; server
    /// must not parse a request into this or emit `OK finish` itself.
    GetFinish,
}
