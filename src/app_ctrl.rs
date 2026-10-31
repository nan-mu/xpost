//! Firmware App-mode control helpers used by board examples' `xpost-serial` server
//! callbacks. Maps / XDP / perf only — wire framing stays in the example.

use crate::error::{MapUseError, XdpLeaseError, XdpReleaseError, XdpSwapError};
use crate::map::{BPF_ANY, MAP_RT, MapIdx, MapRtGuard, MapType, dump};
use crate::xdp::{QuiescePolicy, SwitchReport, XdpStatus, XDP_RT};

/// Map-domain failure for App clear/get/update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppMapError {
    Busy,
    NoRt,
    NameNotFound,
    InvalidName,
    InvalidKey,
    InvalidValue,
    Full,
    Unsupported,
    Logic,
}

/// XDP enable/disable failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppXdpError {
    Busy,
    NoRt,
    Logic,
}

impl From<MapUseError> for AppMapError {
    fn from(e: MapUseError) -> Self {
        match e {
            MapUseError::NotFound => AppMapError::NameNotFound,
            MapUseError::TypeMismatch | MapUseError::InvalidArg => AppMapError::Unsupported,
            MapUseError::Exists => AppMapError::Logic,
            MapUseError::NoMem => AppMapError::Full,
            MapUseError::Busy => AppMapError::Busy,
        }
    }
}

/// Lease the primary XDP image (idempotent if already leased).
pub fn enable_xdp() -> Result<(), AppXdpError> {
    let xdp_lock = XDP_RT.try_get().ok_or(AppXdpError::NoRt)?;
    let mut xdp = xdp_lock.try_lock().map_err(|_| AppXdpError::Busy)?;
    let map_lock = MAP_RT.try_get().ok_or(AppXdpError::NoRt)?;
    let mut maps = map_lock.try_lock().map_err(|_| AppXdpError::Busy)?;
    match xdp.lease(Some(&mut maps)) {
        Ok(()) => Ok(()),
        Err(XdpLeaseError::Leased) => Ok(()),
        Err(_) => Err(AppXdpError::Logic),
    }
}

fn map_swap(err: XdpSwapError) -> AppXdpError {
    match err {
        XdpSwapError::Busy => AppXdpError::Busy,
        _ => AppXdpError::Logic,
    }
}

fn with_xdp<T>(body: impl FnOnce(&mut crate::xdp::XdpRt) -> Result<T, XdpSwapError>) -> Result<T, AppXdpError> {
    let xdp_lock = XDP_RT.try_get().ok_or(AppXdpError::NoRt)?;
    let mut xdp = xdp_lock.try_lock().map_err(|_| AppXdpError::Busy)?;
    body(&mut xdp).map_err(map_swap)
}

/// A helper may hold `MAP_RT` for one packet. Retry that collision only.
fn with_xdp_retry<T>(
    mut body: impl FnMut(&mut crate::xdp::XdpRt) -> Result<T, XdpSwapError>,
) -> Result<T, AppXdpError> {
    let mut last = AppXdpError::Busy;
    for _ in 0..8 {
        match with_xdp(&mut body) {
            Err(AppXdpError::Busy) => {
                last = AppXdpError::Busy;
                core::hint::spin_loop();
            }
            other => return other,
        }
    }
    Err(last)
}

/// Copy `name` into `slot` while the leased image keeps running.
pub fn stage_xdp(name: &str, slot: u8) -> Result<u32, AppXdpError> {
    with_xdp_retry(|xdp| {
        let id = xdp
            .images
            .iter()
            .position(|img| img.meta.name == name)
            .ok_or(XdpSwapError::UnknownImage)?;
        xdp.stage(id, usize::from(slot))
    })
}

/// Quiesce and publish a staged slot. `pass` selects the quiesce policy.
pub fn activate_xdp(slot: u8, generation: u32, pass: bool) -> Result<SwitchReport, AppXdpError> {
    let policy = if pass {
        QuiescePolicy::Pass
    } else {
        QuiescePolicy::Drop
    };
    with_xdp_retry(|xdp| xdp.activate(usize::from(slot), generation, policy))
}

/// Quiesce and publish the retained slot.
pub fn rollback_xdp(pass: bool) -> Result<SwitchReport, AppXdpError> {
    let policy = if pass {
        QuiescePolicy::Pass
    } else {
        QuiescePolicy::Drop
    };
    with_xdp_retry(|xdp| xdp.rollback(policy))
}

/// Boot id, slot roles and quiesce counts.
pub fn xdp_status() -> Result<XdpStatus, AppXdpError> {
    let xdp_lock = XDP_RT.try_get().ok_or(AppXdpError::NoRt)?;
    let xdp = xdp_lock.try_lock().map_err(|_| AppXdpError::Busy)?;
    Ok(xdp.status())
}

/// Release the leased XDP image (idempotent if not leased).
pub fn disable_xdp() -> Result<(), AppXdpError> {
    let xdp_lock = XDP_RT.try_get().ok_or(AppXdpError::NoRt)?;
    let mut xdp = xdp_lock.try_lock().map_err(|_| AppXdpError::Busy)?;
    match xdp.release() {
        Ok(()) => Ok(()),
        Err(XdpReleaseError::NotLeased) => Ok(()),
    }
}

/// Lock [`MAP_RT`] and resolve `name` (for single-lock get-map streaming).
pub fn lock_map(name: &str) -> Result<(MapRtGuard<'static>, MapIdx), AppMapError> {
    if name.is_empty() {
        return Err(AppMapError::InvalidName);
    }
    let lock = MAP_RT.try_get().ok_or(AppMapError::NoRt)?;
    let maps = lock.try_lock().map_err(|_| AppMapError::Busy)?;
    let idx = maps.find(name).map_err(|e| match e {
        MapUseError::NotFound => AppMapError::NameNotFound,
        MapUseError::Busy => AppMapError::Busy,
        _ => AppMapError::Logic,
    })?;
    Ok((maps, idx))
}

/// Clear a named map (ARRAY → zero fill; HASH → drop entries).
pub fn clear_map(name: &str) -> Result<(), AppMapError> {
    let (mut maps, idx) = lock_map(name)?;
    let ty = maps.get(idx)?.map_type();
    match ty {
        MapType::Array => maps.array_clear(idx)?,
        MapType::Hash => maps.hash_clear(idx)?,
    }
    Ok(())
}

/// Update one map entry from App wire tokens.
///
/// ARRAY: `key` = decimal slot, `value` = decimal `u64`.
/// HASH: `key` = dotted IPv4, `value` = decimal integer (truncated to value size).
pub fn update_map(name: &str, key: &[u8], value: &[u8]) -> Result<(), AppMapError> {
    if name == "lookup_timing" {
        let _ = key;
        return set_lookup_timing(value);
    }
    let (mut maps, idx) = lock_map(name)?;
    let ty = maps.get(idx)?.map_type();
    let val = parse_u64_token(value).ok_or(AppMapError::InvalidValue)?;
    match ty {
        MapType::Array => {
            let slot = parse_u32_token(key).ok_or(AppMapError::InvalidKey)?;
            maps.array_set_u64(idx, slot, val).map_err(|e| match e {
                MapUseError::InvalidArg | MapUseError::NotFound => AppMapError::InvalidKey,
                other => other.into(),
            })
        }
        MapType::Hash => {
            let ip = parse_ipv4_token(key).ok_or(AppMapError::InvalidKey)?;
            let vs = maps.hash_value_size(idx)?;
            let mut val_buf = [0u8; 8];
            if vs == 0 || vs > val_buf.len() {
                return Err(AppMapError::Unsupported);
            }
            val_buf[..vs].copy_from_slice(&val.to_le_bytes()[..vs]);
            maps.hash_update(idx, &ip, &val_buf[..vs], BPF_ANY)
                .map_err(|e| match e {
                    MapUseError::InvalidArg => AppMapError::InvalidKey,
                    MapUseError::NoMem => AppMapError::Full,
                    other => other.into(),
                })
        }
    }
}

/// Set the lookup histogram gate. The image must be released.
/// Firmware built without `perf-lookup` rejects the command.
fn set_lookup_timing(value: &[u8]) -> Result<(), AppMapError> {
    #[cfg(not(feature = "perf-lookup"))]
    {
        let _ = value;
        return Err(AppMapError::Unsupported);
    }
    #[cfg(feature = "perf-lookup")]
    {
        let on = match value {
            b"0" => false,
            b"1" => true,
            _ => return Err(AppMapError::InvalidValue),
        };
        let xdp_lock = XDP_RT.try_get().ok_or(AppMapError::NoRt)?;
        let xdp = xdp_lock.try_lock().map_err(|_| AppMapError::Busy)?;
        if xdp.leased() {
            return Err(AppMapError::Busy);
        }
        crate::ext::perf::aggregate::set_lookup_timing(on);
        Ok(())
    }
}

/// Parse a decimal `u32` wire token.
pub fn parse_u32_token(tok: &[u8]) -> Option<u32> {
    if tok.is_empty() || tok.len() > 10 {
        return None;
    }
    let mut n = 0u32;
    for &b in tok {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
    }
    Some(n)
}

/// Parse a decimal `u64` wire token.
pub fn parse_u64_token(tok: &[u8]) -> Option<u64> {
    if tok.is_empty() || tok.len() > 20 {
        return None;
    }
    let mut n = 0u64;
    for &b in tok {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(n)
}

/// Parse a dotted IPv4 wire token into network-order bytes.
pub fn parse_ipv4_token(tok: &[u8]) -> Option<[u8; 4]> {
    let s = core::str::from_utf8(tok).ok()?;
    let mut out = [0u8; 4];
    let mut parts = 0u8;
    let mut acc = 0u32;
    let mut digits = 0u8;
    for b in s.bytes() {
        if b == b'.' {
            if digits == 0 || parts >= 3 {
                return None;
            }
            out[parts as usize] = acc as u8;
            acc = 0;
            digits = 0;
            parts += 1;
        } else if b.is_ascii_digit() {
            digits += 1;
            if digits > 3 {
                return None;
            }
            acc = acc * 10 + u32::from(b - b'0');
            if acc > 255 {
                return None;
            }
        } else {
            return None;
        }
    }
    if parts != 3 || digits == 0 {
        return None;
    }
    out[3] = acc as u8;
    Some(out)
}

/// Re-export dump helpers HAL examples use for software maps.
pub use dump::{
    array_blob_len, format_hash_key, hash_entry_len, hash_header_len, value_as_u64,
    write_array_blob, write_hash_entry, write_hash_header,
};
