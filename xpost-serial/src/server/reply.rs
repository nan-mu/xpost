//! Build wire replies for the server App.
//!
//! TODO: re-enable builders once they write via `ServerError` / `OkReply` without `alloc`.
//! Previous API used owned `String`/`Vec` and has been commented out.

use core::fmt;

use crate::r#type::reply::ServerError;

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => f.write_str("ERR unknown"),
        }
    }
}

/*
use crate::r#type::reply::{OkReply, ServerError};

pub mod ok {
    use super::*;

    pub fn ping() -> OkReply<'static> {
        OkReply::Ping
    }
    // … get_map should for-loop write entries at the call site …
}

pub fn unknown() -> ServerError {
    ServerError::Unknown
}
*/

/// Placeholder so the module compiles while builders are stubbed.
pub fn stub_todo() {}
