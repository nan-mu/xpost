//! DUT / App-mode side: parse request lines and build replies.

mod reply;

pub use reply::{err, ok, unknown};

use crate::r#type::command::{Command, ParseCommandError, parse_command};

/// Parse one App-mode request line into a wire [`Command`].
///
/// Unknown → `Err(ParseCommandError::Unknown)` (caller should reply `ERR unknown`).
pub fn dispatch(line: &str) -> Result<Command, ParseCommandError> {
    parse_command(line)
}
