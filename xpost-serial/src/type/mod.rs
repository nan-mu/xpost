//! App-mode wire protocol shapes. `no_std`, no `alloc`.
//!
//! One request line + `\n`. Text replies start with `OK` / `ERR`. Tokens are
//! whitespace-separated; request verbs are ignore-ascii-case. Unknown request →
//! `ERR unknown`. Host flash ops are **not** App-mode lines (see client `FlashOp`).
//!
//! Parsing / formatting are **not** in this module: server parses requests and
//! builds replies; client formats requests and parses replies.
//!
//! # [`command::Command`] — request line
//!
//! | Variant | Wire |
//! |---------|------|
//! | [`Ping`](command::Command::Ping) | `ping` |
//! | [`EnableXdp`](command::Command::EnableXdp) | `enable xdp` |
//! | [`DisableXdp`](command::Command::DisableXdp) | `disable xdp` |
//! | [`ClearMap`](command::Command::ClearMap) | `clear map <name>` |
//! | [`GetMap`](command::Command::GetMap) | `get map <name>` |
//! | [`UpdateMap`](command::Command::UpdateMap) | `update map <name> <key> <value>` |
//! | [`ClearPerf`](command::Command::ClearPerf) | `clear perf` |
//! | [`GetPerf`](command::Command::GetPerf) | `get perf` |
//!
//! `UpdateMap`: ARRAY `key` = decimal slot, `value` = `u64` token; HASH `key` =
//! dotted IPv4 (as bytes), `value` = `u64` token.
//!
//! # [`reply::OkReply`] — success
//!
//! | Variant | Wire |
//! |---------|------|
//! | [`Ping`](reply::OkReply::Ping) | `OK ping` |
//! | [`EnableXdp`](reply::OkReply::EnableXdp) | `OK enable xdp` |
//! | [`DisableXdp`](reply::OkReply::DisableXdp) | `OK disable xdp` |
//! | [`ClearMap`](reply::OkReply::ClearMap) | `OK clear map` |
//! | [`ClearPerf`](reply::OkReply::ClearPerf) | `OK clear perf` |
//! | [`UpdateMap`](reply::OkReply::UpdateMap) | `OK update map` |
//! | [`GetMap`](reply::OkReply::GetMap) | `OK get map <n>` + `n` blob bytes |
//! | [`GetPerf`](reply::OkReply::GetPerf) | `OK get perf <n>` + `n` blob bytes |
//! | [`Init`](reply::OkReply::Init) | `OK init` (unsolicited; see below) |
//!
//! **`get map` / `get perf` success (length-prefixed blob)**
//!
//! Text header line, then exactly `n` binary bytes (not line-oriented after the
//! header):
//!
//! ```text
//! OK get map <n>
//! <n bytes>
//! ```
//!
//! ```text
//! OK get perf <n>
//! <n bytes>
//! ```
//!
//! `get perf` blob = `ext::perf` layout (`n` ↔ feature mask). `get map` blob
//! layout is defined by the server map dump encoder. If the reply is `ERR …`
//! instead of the header, there is no blob.
//!
//! **`OK init` (unsolicited)**
//!
//! Not triggered by any [`command::Command`]. After App-mode initialization
//! finishes, the server emits `OK init` on its own so the host can detect
//! settle / ready (e.g. post-flash boot). Treat as a spontaneous UART line,
//! not a `request` reply.
//!
//! # [`reply::ServerError`] — DUT `ERR`
//!
//! Wire: `ERR <cmd-ish> <why>` (e.g. `ERR clear map not-found`,
//! `ERR get perf no-perf`). Recognized shape collapses to
//! [`ServerError::Unknown`](reply::ServerError::Unknown) for now (`why` discarded).
//! Local host parse failures are a separate [`reply::ErrReply`] implementor on
//! the client (`client::error::ParseError`), not a wire variant. App-mode
//! `request` returns `Result<OkReply, E>` with `E: ErrReply`.

pub mod command;
pub mod reply;

pub use command::Command as WireCommand;
pub use reply::{ErrReply, MapEntry, MapKey, MapKind, OkReply, ServerError};
