//! `ERR …` reply parsers.
//!
//! Interface only for now — bodies TBD.

use crate::client::error::ParseError;
use crate::r#type::command::Command;
use crate::r#type::reply::ServerError;

/// Parse a DUT `ERR …` line in the context of the outbound [`Command`].
///
/// Not implemented yet.
pub fn parse_err_reply(
    input: &[u8],
    cmd: &Command<'_>,
) -> Result<ServerError, ParseError> {
    let _ = (input, cmd);
    Err(ParseError::InvalidSyntax)
}
