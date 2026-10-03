//! Build wire [`Reply`] values for the DUT App.

use alloc::string::String;
use alloc::vec::Vec;

use crate::r#type::reply::{ErrReply, MapDump, MapEntry, MapKind, OkReply, Reply};

pub mod ok {
    use super::*;

    pub fn ping() -> Reply {
        Reply::Ok(OkReply::Ping)
    }
    pub fn enable_xdp() -> Reply {
        Reply::Ok(OkReply::EnableXdp)
    }
    pub fn disable_xdp() -> Reply {
        Reply::Ok(OkReply::DisableXdp)
    }
    pub fn clear_map() -> Reply {
        Reply::Ok(OkReply::ClearMap)
    }
    pub fn clear_perf() -> Reply {
        Reply::Ok(OkReply::ClearPerf)
    }
    pub fn update_map() -> Reply {
        Reply::Ok(OkReply::UpdateMap { n: None })
    }
    pub fn update_map_n(n: u32) -> Reply {
        Reply::Ok(OkReply::UpdateMap { n: Some(n) })
    }
    pub fn get_perf(len: usize) -> Reply {
        Reply::Ok(OkReply::GetPerf { len })
    }
    pub fn get_map_array(name: &str, size: u32, entries: Vec<MapEntry>) -> Reply {
        Reply::Ok(OkReply::GetMap(MapDump {
            name: String::from(name),
            kind: MapKind::Array { size },
            entries,
        }))
    }
    pub fn get_map_hash(name: &str, size: u32, cap: u32, entries: Vec<MapEntry>) -> Reply {
        Reply::Ok(OkReply::GetMap(MapDump {
            name: String::from(name),
            kind: MapKind::Hash { size, cap },
            entries,
        }))
    }
}

pub fn err(cmd: &str, why: &str) -> Reply {
    Reply::Err(ErrReply {
        cmd: String::from(cmd),
        why: String::from(why),
    })
}

/// `ERR unknown` (no why token).
pub fn unknown() -> Reply {
    Reply::Err(ErrReply {
        cmd: String::from("unknown"),
        why: String::new(),
    })
}
