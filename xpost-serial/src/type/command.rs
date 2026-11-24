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

pub const EXEC_BATCH_BYTES: usize = 8258;
pub const EXEC_BATCH_WRITE_MAX_BYTES: usize = 256;
pub const EXEC_BATCH_READ_MAX_BYTES: usize = 512;

#[cfg(any(feature = "client", feature = "server"))]
pub(crate) fn is_exec_batch_hex(data: &[u8]) -> bool {
    !data.is_empty()
        && data.len() <= 2 * EXEC_BATCH_WRITE_MAX_BYTES
        && data.len() % 2 == 0
        && data.iter().all(u8::is_ascii_hexdigit)
}

/// Capture installed by `perf prepare`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfVariant {
    Metrics,
    Trace,
}

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
    /// `exec batch reset`
    ExecBatchReset,
    /// `exec batch write <offset> <hex>`. `data` borrows the ASCII hex token.
    ExecBatchWrite {
        offset: u32,
        data: &'a [u8],
    },
    /// `exec batch invoke`
    ExecBatchInvoke,
    /// `exec batch read <offset> <max_bytes>`.
    ExecBatchRead {
        offset: u32,
        max_bytes: u32,
    },
    ClearMap {
        name: &'a str,
    },
    GetMap {
        name: &'a str,
    },
    /// Names borrow ASCII; `key` / `value` are opaque wire byte tokens (not
    /// decoded by this crate). Firmware validates them against the map.
    UpdateMap {
        name: &'a str,
        key: &'a [u8],
        value: &'a [u8],
    },
    ClearPerf,
    GetPerf,
    /// `perf prepare <32 hex uuid> <metrics|trace> <64 hex profile>`.
    ///
    /// `uuid` is 16 raw bytes and `profile` is 32 raw bytes. The wire form is hex.
    PerfPrepare {
        uuid: [u8; 16],
        kind: PerfVariant,
        profile: [u8; 32],
    },
    /// `perf arm <epoch>`
    PerfArm {
        epoch: u32,
    },
    /// `perf freeze <epoch>`
    PerfFreeze {
        epoch: u32,
    },
    /// `perf read <epoch> <offset> <max_bytes>`.
    ///
    /// `max_bytes` above 512 is a legal request. The session clamps the copy.
    PerfRead {
        epoch: u32,
        offset: u32,
        max_bytes: u32,
    },
    /// `perf release <epoch>`
    PerfRelease {
        epoch: u32,
    },
    /// `xdp flash <name> <slot>` — copy a cold image into a slot.
    ///
    /// A non-running slot stays staged. The active primary slot is replaced
    /// in place and resumes at the same entry.
    XdpFlash {
        name: &'a str,
        slot: u8,
    },
    /// `xdp switch <slot> <generation> <drop|pass>`.
    XdpSwitch {
        slot: u8,
        generation: u32,
        pass: bool,
    },
    /// `get xdp` — boot id, slot roles, quiesce counts. Not the cold-table list.
    GetXdpStatus,
    /// Wait for unsolicited `OK init` in the host record. **Not a wire verb** —
    /// must not be sent on UART; server must not parse a request into this.
    GetInit,
    /// Wait for unsolicited `OK finish` in the host record (e.g. eBPF
    /// `bpf_printk` or HAL App raw UART). **Not a wire verb** — must not be
    /// sent on UART; server must not parse a request into this or emit
    /// `OK finish` via the reply path.
    GetFinish,
}
