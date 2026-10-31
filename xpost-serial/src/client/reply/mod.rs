//! Host-side parse of wire replies (`OK …` / `ERR …`) via [winnow](https://github.com/winnow-rs/winnow).
//!
//! | Module | Role |
//! |--------|------|
//! | [`ok`] | `OK` fixed (`fixed_ok!`) + variable (`get map` / `get perf`) |
//! | [`err`] | `ERR` domains (server / map / perf / syntax) |
//! | [`map`] / [`perf`] | temporary variable-OK bodies (to fold into [`ok`]) |

pub mod err;
pub mod map;
pub mod ok;
pub mod perf;

pub use err::{
    parse_err_reply, parse_map_err_reply, parse_perf_err_reply, parse_syntax_err_reply,
};
pub use ok::{
    parse_clear_map_reply, parse_clear_perf_reply, parse_disable_exec_reply,
    parse_disable_xdp_reply, parse_enable_exec_reply, parse_enable_xdp_reply, parse_finish_reply,
    parse_get_map_len, parse_get_map_reply, parse_get_perf_len, parse_get_perf_reply,
    parse_get_xdp_status_reply, parse_init_reply, parse_ping_reply, parse_update_map_reply,
    parse_xdp_activate_reply, parse_xdp_rollback_reply, parse_xdp_stage_reply,
};
