//! App-mode wire protocol shapes. `no_std`, no `alloc`.
//!
//! One request line + `\n`. Text replies start with `OK` / `ERR`. Tokens are
//! whitespace-separated; request verbs are ignore-ascii-case. Unknown request →
//! `ERR unknown`. Host flash ops are **not** App-mode lines (see client `FlashOp`).
//!
//! Parsing / formatting are **not** in this module: server parses requests and
//! builds replies; client formats requests and parses replies.
//!
//! # [`command::Command`] — request / host wait
//!
//! | Variant | Wire / host |
//! |---------|-------------|
//! | [`Ping`](command::Command::Ping) | `ping` |
//! | [`EnableXdp`](command::Command::EnableXdp) | `enable xdp` |
//! | [`DisableXdp`](command::Command::DisableXdp) | `disable xdp` |
//! | [`EnableExec`](command::Command::EnableExec) | `enable exec` |
//! | [`DisableExec`](command::Command::DisableExec) | `disable exec` |
//! | [`ClearMap`](command::Command::ClearMap) | `clear map <name>` |
//! | [`GetMap`](command::Command::GetMap) | `get map <name>` |
//! | [`UpdateMap`](command::Command::UpdateMap) | `update map <name> <key> <value>` |
//! | [`ClearPerf`](command::Command::ClearPerf) | `clear perf` |
//! | [`GetPerf`](command::Command::GetPerf) | `get perf` |
//! | [`GetInit`](command::Command::GetInit) | **no wire** — host record wait only |
//! | [`GetFinish`](command::Command::GetFinish) | **no wire** — host record wait only |
//!
//! `UpdateMap`: `key` and `value` are **opaque wire byte tokens**. xpost-serial
//! does not decode them into integers, IPs, or workload structs — firmware
//! validates them against the selected map. (Common App conventions such as
//! ARRAY decimal slots or HASH dotted-IPv4 keys live in firmware / host parse,
//! not in this crate's types.)
//!
//! **[`GetInit`](command::Command::GetInit)** / **[`GetFinish`](command::Command::GetFinish)**
//! must never be written to UART as request lines and must never be produced by
//! server request parsing. Host `request(GetInit|GetFinish)` only looks for /
//! waits until `OK init` / `OK finish` appear in the transcript (deadlines
//! [`command::GET_INIT_TIMEOUT`] / [`command::GET_FINISH_TIMEOUT`]). Success is
//! [`OkReply::Init`] / [`OkReply::Finish`]. The serial **reply path** must not
//! emit `OK finish` via [`OkReply::Finish`](reply::OkReply::Finish); an
//! unsolicited UART line from eBPF `bpf_printk` (or a HAL App twin writing the
//! same raw line) is how the transcript gets that marker.
//!
//! # [`reply::OkReply`] — success
//!
//! | Variant | Wire |
//! |---------|------|
//! | [`Ping`](reply::OkReply::Ping) | `OK ping` |
//! | [`EnableXdp`](reply::OkReply::EnableXdp) | `OK enable xdp` |
//! | [`DisableXdp`](reply::OkReply::DisableXdp) | `OK disable xdp` |
//! | [`EnableExec`](reply::OkReply::EnableExec) | `OK enable exec` |
//! | [`DisableExec`](reply::OkReply::DisableExec) | `OK disable exec` |
//! | [`ClearMap`](reply::OkReply::ClearMap) | `OK clear map` |
//! | [`ClearPerf`](reply::OkReply::ClearPerf) | `OK clear perf` |
//! | [`UpdateMap`](reply::OkReply::UpdateMap) | `OK update map` |
//! | [`GetMap`](reply::OkReply::GetMap) | `OK get map <n>` + `n` blob bytes |
//! | [`GetPerf`](reply::OkReply::GetPerf) | `OK get perf <n>` + `n` blob bytes |
//! | [`Init`](reply::OkReply::Init) | `OK init` (unsolicited; see below) |
//! | [`Finish`](reply::OkReply::Finish) | `OK finish` (unsolicited; see below) |
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
//! After App-mode initialization finishes, the server emits `OK init` on its
//! own (no request line). The host observes it via
//! [`Command::GetInit`](command::Command::GetInit) as a **record wait** only —
//! never by sending a command on the wire. Deadline:
//! [`command::GET_INIT_TIMEOUT`].
//!
//! **`OK finish` (unsolicited transcript line)**
//!
//! Completion of a function-call / `exec` run is signaled by an unsolicited
//! `OK finish` line on the UART transcript (eBPF `bpf_printk`, or a HAL App
//! writing the same raw bytes). The serial **reply path must not** emit this
//! via [`OkReply::Finish`](reply::OkReply::Finish). The host observes it via
//! [`Command::GetFinish`](command::Command::GetFinish) as a **record wait** only.
//! Deadline: [`command::GET_FINISH_TIMEOUT`].
//!
//! # Device error contract
//!
//! Errors are flat semantic types sharing the [`reply::ErrReply`] marker:
//! [`reply::ServerError`], [`reply::MapError`] and [`reply::PerfError`] each
//! implement it directly. None wraps the others. There are no per-command
//! error variants, catch-all wrapper enums, or implicit domain-to-server
//! conversions. Request syntax errors also implement the same marker directly.
//!
//! Every wire error is one LF-terminated text line with no binary payload.
//! Its tokens identify the failure's meaning/domain, not the originating command:
//!
//! - General errors: `ERR unknown`, `ERR timeout`, `ERR logic`, `ERR busy`,
//!   `ERR unsupported`. `unknown` includes unrecognized/empty requests and
//!   the host-only `get init`; firmware may instead ignore empty UART lines.
//! - Map errors: `ERR map name-not-found`, `ERR map key-not-found`,
//!   `ERR map invalid-name`, `ERR map invalid-key`, `ERR map invalid-value`,
//!   `ERR map full`, `ERR map unsupported`, `ERR map buffer-too-small`.
//! - Perf errors: `ERR perf unavailable`, `ERR perf unsupported`,
//!   `ERR perf buffer-too-small`.
//! - Request syntax errors: `ERR bad-arity` for missing/extra arguments and
//!   `ERR invalid-utf8` for a map-name token that cannot be decoded as text,
//!   `ERR line-too-long` after an oversized UART request has been discarded.
//!
//! For example, get/clear/update of an absent named map all use
//! [`MapError::NameNotFound`]. A missing key is [`MapError::KeyNotFound`]; a hash
//! upsert that permits insertion does not fail merely because its key is absent.
//! A full hash insertion uses [`MapError::Full`], whereas insufficient snapshot
//! storage uses [`MapError::BufferTooSmall`]. Get/clear of unavailable performance
//! data uses [`PerfError::Unavailable`]. Any operation's device execution deadline
//! uses [`ServerError::Timeout`]; violated execution invariants use
//! [`ServerError::Logic`], independently of the command or resource domain.
//!
//! A timeout is a device execution result, not a host receive timeout. Lack of
//! response is never itself a wire error. A local host error may implement
//! [`reply::ErrReply`] without any wire representation. Server-side wire codecs
//! provide encoding separately; neither `Debug` nor `Display` is required.
//!
//! The firmware validates names, key/value tokens and resource state, executes
//! commands, and enforces any device deadlines. The protocol codec does not
//! access maps, run timers, or guarantee rollback: a failed clear/update may
//! already have changed state. Retries are the caller's policy.
//!
//! Validate resource availability and payload length before sending `OK get map <n>\n` or
//! `OK get perf <n>\n`. Once that header starts, the sender must complete exactly
//! `n` bytes; an execution or I/O failure cannot be reported by appending `ERR`
//! inside the payload. The firmware may then stream an infallible encoder such
//! as `xpost::ext::perf::report_perf` through the server writer without allocating
//! a full blob. The UART owner must recover/re-synchronize a broken
//! transport before exchanging further replies.
//!
//! Local host parse failures implement [`reply::ErrReply`] separately from wire
//! errors, as do host waiting timeouts. The client parses flat wire ERR domains
//! ([`reply::ServerError`], [`reply::MapError`], [`reply::PerfError`],
//! [`reply::SyntaxError`]) without folding one into another.

pub mod command;
pub mod reply;

pub use command::{Command as WireCommand, GET_FINISH_TIMEOUT, GET_INIT_TIMEOUT};
pub use reply::{
    ErrReply, MapEntry, MapError, MapKey, MapKind, OkReply, PerfError, ServerError, SyntaxError,
};
