//! App-mode wire protocol + host control-plane shapes.
//!
//! `no_std` + `alloc`. Wire commands are one line + `\n`; replies start with `OK` / `ERR`.

pub mod command;
pub mod ops;
pub mod reply;

pub use command::{Command as WireCommand, ParseCommandError, parse_command};
pub use ops::{
    Chip, Clear, Command, Disable, Enable, Flash, FlashOp, Get, Update,
};
pub use reply::{
    ErrReply, MapDump, MapEntry, MapKind, OkReply, ParseReplyError, Reply, parse_ok_get_perf_len,
    parse_reply_line,
};
