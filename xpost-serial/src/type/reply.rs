//! Wire reply shapes: `OK …` / `ERR …`. `no_std`, no `alloc`.
//!
//! Host parsing lives on the client; server-side builders live on the server.
//! Wire conversion uses explicit codec interfaces; these shapes do not require
//! `Debug` or `Display`.

/// Successful App-mode reply (borrows wire bytes from the caller / record store).
pub enum OkReply<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    EnableExec,
    DisableExec,
    ClearMap,
    ClearPerf,
    /// `OK get map <n>` + `n` blob bytes (same framing as [`Self::GetPerf`]).
    GetMap {
        blob: &'a [u8],
    },
    UpdateMap,
    /// `OK get perf <n>` + `n` blob bytes.
    GetPerf {
        blob: &'a [u8],
    },
    /// Unsolicited `OK init` after App-mode init finishes.
    ///
    /// Host obtains this via [`crate::command::Command::GetInit`] as a
    /// transcript wait only — there is no UART request line.
    Init,
    /// Unsolicited `OK finish` (e.g. from eBPF `bpf_printk` on UART).
    ///
    /// Host obtains this via [`crate::command::Command::GetFinish`] as a
    /// transcript wait only — there is no UART request line. The serial
    /// **server must not** emit this reply.
    Finish,
}

/// Common bound for independent reply-error types.
///
/// Server, map, perf and local parsing errors implement this trait directly.
/// There is no enclosing error enum and no requirement to convert one error
/// domain into another. Formatting and wire encoding are separate interfaces;
/// implementing this marker alone does not make a local error a wire reply.
pub trait ErrReply {}

/// General device execution/protocol failures, independent of the command.
///
/// Domain failures are peers ([`MapError`] and [`PerfError`]), never payloads of
/// this enum. See the [wire contract](super#device-error-contract). A timeout
/// reported by the device is distinct from the host's local waiting deadline.
#[derive(Clone, Copy)]
pub enum ServerError {
    /// `ERR unknown`: an empty or unrecognized request, including `get init`.
    Unknown,
    /// `ERR timeout`: the device execution deadline expired before output began.
    /// Does not imply rollback of work already performed.
    Timeout,
    /// `ERR logic`: an internal invariant or execution-state assumption failed.
    Logic,
    /// `ERR busy`: a conflicting operation prevents execution at this time.
    Busy,
    /// `ERR unsupported`: the requested facility/operation is not implemented.
    Unsupported,
}

impl ErrReply for ServerError {}

/// Map-domain failures shared by get, clear and update operations.
///
/// These describe what failed, without wrapping an operation or another error.
/// General device timeouts and logic failures remain [`ServerError`] values.
#[derive(Clone, Copy)]
pub enum MapError {
    /// `ERR map name-not-found`: no map exists under the requested name.
    NameNotFound,
    /// `ERR map key-not-found`: an operation requiring an existing key found none.
    /// An ordinary hash upsert may insert an absent key and need not fail here.
    KeyNotFound,
    /// `ERR map invalid-name`: the name violates firmware naming rules.
    InvalidName,
    /// `ERR map invalid-key`: malformed/wrong-sized key or out-of-range index.
    InvalidKey,
    /// `ERR map invalid-value`: malformed, overflowing or wrong-sized value.
    InvalidValue,
    /// `ERR map full`: a new hash entry cannot fit in the map.
    Full,
    /// `ERR map unsupported`: the map kind/layout does not support the operation.
    Unsupported,
    /// `ERR map buffer-too-small`: bounded storage cannot hold the map snapshot.
    BufferTooSmall,
}

impl ErrReply for MapError {}

/// Performance-domain failures shared by get and clear operations.
///
/// General device timeouts and logic failures remain [`ServerError`] values.
#[derive(Clone, Copy)]
pub enum PerfError {
    /// `ERR perf unavailable`: the required performance facility is not enabled.
    Unavailable,
    /// `ERR perf unsupported`: the requested perf operation/layout is unsupported.
    Unsupported,
    /// `ERR perf buffer-too-small`: bounded storage cannot hold the report.
    BufferTooSmall,
}

impl ErrReply for PerfError {}

/// Request-line syntax failures (not map/perf/domain execution).
///
/// Firmware request parsers emit these wire lines for arity/utf8/length
/// problems. Empty/unknown requests use [`ServerError::Unknown`] instead.
#[derive(Clone, Copy)]
pub enum SyntaxError {
    /// `ERR bad-arity`
    BadArity,
    /// `ERR invalid-utf8`
    InvalidUtf8,
    /// `ERR line-too-long`
    LineTooLong,
}

impl ErrReply for SyntaxError {}

pub enum MapKind {
    Array { size: u32 },
    Hash { size: u32, cap: u32 },
}

/// Map key: array index (`u32`) or hash key bytes.
pub enum MapKey<'a> {
    Array(u32),
    Hash(&'a [u8]),
}

/// One decoded map entry (blob payload decode; not a wire line by itself).
pub struct MapEntry<'a> {
    pub key: MapKey<'a>,
    /// Raw value token (not parsed to integers).
    pub value: &'a [u8],
}
