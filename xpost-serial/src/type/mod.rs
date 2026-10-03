//! App-mode wire protocol. `no_std`, no `alloc`.

pub mod command;
pub mod reply;

pub use command::Command as WireCommand;
pub use reply::{ErrReply, MapEntry, MapKey, MapKind, OkReply, ServerError};
