//! Server / App-mode side: parse request lines and build replies.

mod command;
mod reply;

// Parse / builders temporarily stubbed.
#[allow(unused_imports)]
pub use command::stub_todo as command_stub;
#[allow(unused_imports)]
pub use reply::stub_todo;

pub use command::ParseCommandError;

use crate::r#type::command::Command;

/// Parse one App-mode request line into a wire [`Command`].
///
/// Temporarily stubbed — see [`command`] module (commented parse).
pub fn dispatch(_line: &[u8]) -> Result<Command<'_>, ParseCommandError> {
    Err(ParseCommandError::Unknown)
}
