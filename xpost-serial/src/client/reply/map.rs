//! Host parse helpers for `get map` replies (`OK get map <n>` + blob).

use winnow::{
    ascii::{Caseless, dec_uint, multispace0, space0, space1},
    combinator::eof,
    error::{ContextError, ErrMode},
    ModalResult, Parser,
};

use crate::client::error::ParseError;
use crate::r#type::reply::OkReply;

/// Complete framed `OK get map <n>\n` + exactly `n` blob bytes.
pub fn parse_ok_get_map(raw: &[u8]) -> Result<OkReply<'_>, ParseError> {
    let mut i = raw;
    let blob = parse_ok_get_map_m(&mut i).map_err(|_| ParseError::BadShape("get-map"))?;
    Ok(OkReply::GetMap { blob })
}

fn parse_ok_get_map_m<'a>(i: &mut &'a [u8]) -> ModalResult<&'a [u8]> {
    multispace0.parse_next(i)?;
    b"OK".parse_next(i)?;
    space1.parse_next(i)?;
    Caseless(b"get").parse_next(i)?;
    space1.parse_next(i)?;
    Caseless(b"map").parse_next(i)?;
    space1.parse_next(i)?;
    let n: usize = dec_uint.parse_next(i)?;
    b"\n".parse_next(i)?;
    if i.len() != n {
        return Err(ErrMode::Backtrack(ContextError::new()));
    }
    Ok(*i)
}

/// Parse declared length from an `OK get map <n>` header line (framing only).
pub fn parse_ok_get_map_len(line: &[u8]) -> Option<usize> {
    let mut i = line;
    parse_ok_get_map_len_m(&mut i).ok()
}

fn parse_ok_get_map_len_m(i: &mut &[u8]) -> ModalResult<usize> {
    multispace0.parse_next(i)?;
    b"OK".parse_next(i)?;
    space1.parse_next(i)?;
    Caseless(b"get").parse_next(i)?;
    space1.parse_next(i)?;
    Caseless(b"map").parse_next(i)?;
    space1.parse_next(i)?;
    let n: usize = dec_uint.parse_next(i)?;
    space0.parse_next(i)?;
    eof.parse_next(i)?;
    Ok(n)
}
