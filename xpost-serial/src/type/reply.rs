//! Wire reply shapes: `OK …` / `ERR …`. `no_std`, no `alloc`.
//!
//! Host parsing lives on the client; server-side builders live on the server.
//! [`Display`] is optional and implemented beside those sides — not required by
//! [`ErrReply`].

/// Successful App-mode reply (borrows wire bytes from the caller / record store).
pub enum OkReply<'a> {
    Ping,
    EnableXdp,
    DisableXdp,
    ClearMap,
    ClearPerf,
    /// Header only — body lines stay in the record for the caller to for-loop.
    GetMap {
        name: &'a str,
        kind: MapKind,
        entries: &'a [MapEntry<'a>],
    },
    UpdateMap,
    GetPerf {
        blob: &'a [u8],
    },
    /// Host-only ack (e.g. flash). Never appears on the server wire.
    Done,
}

/// Reply-error surface: local parse vs server `ERR` implement separately.
///
/// [`fmt::Display`] is **not** required — wire/host formatting is optional and
/// provided by client / server as needed.
pub trait ErrReply {}

/// Successfully recognized server `ERR <cmd>` (why text is discarded).
pub enum ServerError {
    Unknown,
}

impl ErrReply for ServerError {}

pub enum MapKind {
    Array { size: u32 },
    Hash { size: u32, cap: u32 },
}

/// Map key: array index (`u32`) or hash key bytes.
pub enum MapKey<'a> {
    Array(u32),
    Hash(&'a [u8]),
}

/// One `get map` body line.
pub struct MapEntry<'a> {
    pub key: MapKey<'a>,
    /// Raw value token (not parsed to integers).
    pub value: &'a [u8],
}
