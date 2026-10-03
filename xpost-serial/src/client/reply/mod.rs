//! Host-side parse of wire replies (`OK …` / `ERR …`) via [winnow](https://github.com/winnow-rs/winnow).
//!
//! | Module | Role |
//! |--------|------|
//! | [`ok`] | `OK` fixed (`fixed_ok!`) + variable (`get map` / `get perf`) |
//! | [`err`] | `ERR` general ([`ServerError`](crate::r#type::reply::ServerError)) |
//! | [`map`] / [`perf`] | temporary variable-OK bodies (to fold into [`ok`]) |

pub mod err;
pub mod map;
pub mod ok;
pub mod perf;

pub use err::parse_err_reply;
pub use ok::{
    parse_clear_map_reply, parse_clear_perf_reply, parse_disable_xdp_reply,
    parse_enable_xdp_reply, parse_get_map_len, parse_get_map_reply, parse_get_perf_len,
    parse_get_perf_reply, parse_init_reply, parse_ping_reply, parse_update_map_reply,
};
