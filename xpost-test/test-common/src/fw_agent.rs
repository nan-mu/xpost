//! Hand-rolled codec for [`proto/fw_agent.proto`](../../../proto/fw_agent.proto).
//! Host-only (Mac agent + TR3000 executor).

extern crate std;

use std::string::{String, ToString};
use std::vec::Vec;

pub const STATUS_OK: u32 = 200;
pub const STATUS_BAD: u32 = 400;

pub const AGENT_LIST_PROFILES: u32 = 1;
pub const AGENT_FLASH_PROFILE: u32 = 2;
pub const AGENT_REBOOT: u32 = 3;
pub const AGENT_GET_STATUS: u32 = 4;
/// Install XDP ELF by path (UTF-8 body = absolute or workspace-relative path).
pub const AGENT_PROGRAM: u32 = 10;
/// Erase cold table.
pub const AGENT_CLEAR: u32 = 11;
/// Serial `counters` → OK line in response message.
pub const AGENT_COUNTERS: u32 = 12;
/// UDP stim; body `esp` or `esp:port`; message `ack` / `no-ack`.
pub const AGENT_PROBE: u32 = 13;
/// Fire-and-forget N STIMs; body `esp:port:count`; message `sent=N`.
pub const AGENT_PROBE_BURST: u32 = 14;
/// Serial App-mode request; body = command text; message = OK/ERR line.
pub const AGENT_REQUEST: u32 = 15;

pub const EXEC_SUBMIT_JOB: u32 = 101;
pub const EXEC_GET_JOB: u32 = 102;
pub const EXEC_CANCEL_JOB: u32 = 103;

pub const DROP_PATH_XDP_DROP: u32 = 1;
pub const DROP_PATH_HAL_DROP: u32 = 2;

pub const AGENT_IDLE: u32 = 1;
pub const AGENT_FLASHING: u32 = 2;
pub const AGENT_ERROR: u32 = 3;

pub const JOB_PENDING: u32 = 1;
pub const JOB_FLASHING: u32 = 2;
pub const JOB_RUNNING: u32 = 3;
pub const JOB_DONE: u32 = 4;
pub const JOB_FAILED: u32 = 5;
pub const JOB_CANCELLED: u32 = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadWire,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileInfo {
    pub profile_id: String,
    pub path: u32,
    pub bin_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListProfilesResp {
    pub profiles: Vec<ProfileInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlashProfileResp {
    pub status: u32,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebootResp {
    pub status: u32,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetStatusResp {
    pub state: u32,
    pub last_profile_id: String,
    pub esp_link_ok: bool,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitJobResp {
    pub status: u32,
    pub job_id: u64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetJobResp {
    pub status: u32,
    pub job_state: u32,
    pub message: String,
    pub bench: String,
    pub profile_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelJobResp {
    pub status: u32,
    pub message: String,
}

fn read_varint(buf: &[u8], i: &mut usize) -> Result<u64, DecodeError> {
    let mut x: u64 = 0;
    let mut shift = 0u32;
    loop {
        if *i >= buf.len() {
            return Err(DecodeError::Truncated);
        }
        let b = buf[*i];
        *i += 1;
        x |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(x);
        }
        shift += 7;
        if shift > 63 {
            return Err(DecodeError::BadWire);
        }
    }
}

fn put_varint(out: &mut [u8], mut i: usize, mut x: u64) -> Option<usize> {
    loop {
        if i >= out.len() {
            return None;
        }
        let b = (x as u8) & 0x7f;
        x >>= 7;
        if x == 0 {
            out[i] = b;
            return Some(i + 1);
        }
        out[i] = b | 0x80;
        i += 1;
    }
}

fn skip_field(buf: &[u8], i: &mut usize, wt: u32) -> Result<(), DecodeError> {
    match wt {
        0 => {
            let _ = read_varint(buf, i)?;
        }
        1 => {
            *i = i.checked_add(8).ok_or(DecodeError::Truncated)?;
            if *i > buf.len() {
                return Err(DecodeError::Truncated);
            }
        }
        2 => {
            let n = read_varint(buf, i)? as usize;
            *i = i.checked_add(n).ok_or(DecodeError::Truncated)?;
            if *i > buf.len() {
                return Err(DecodeError::Truncated);
            }
        }
        5 => {
            *i = i.checked_add(4).ok_or(DecodeError::Truncated)?;
            if *i > buf.len() {
                return Err(DecodeError::Truncated);
            }
        }
        _ => return Err(DecodeError::BadWire),
    }
    Ok(())
}

fn read_len_delimited<'a>(buf: &'a [u8], i: &mut usize) -> Result<&'a [u8], DecodeError> {
    let n = read_varint(buf, i)? as usize;
    let end = i.checked_add(n).ok_or(DecodeError::Truncated)?;
    if end > buf.len() {
        return Err(DecodeError::Truncated);
    }
    let slice = &buf[*i..end];
    *i = end;
    Ok(slice)
}

fn put_string(out: &mut [u8], mut i: usize, field: u32, s: &str) -> Option<usize> {
    if s.is_empty() {
        return Some(i);
    }
    i = put_varint(out, i, ((field << 3) | 2) as u64)?;
    i = put_varint(out, i, s.len() as u64)?;
    if i + s.len() > out.len() {
        return None;
    }
    out[i..i + s.len()].copy_from_slice(s.as_bytes());
    Some(i + s.len())
}

fn put_u32(out: &mut [u8], mut i: usize, field: u32, v: u32) -> Option<usize> {
    if v == 0 {
        return Some(i);
    }
    i = put_varint(out, i, ((field << 3) | 0) as u64)?;
    put_varint(out, i, v as u64)
}

fn put_u64(out: &mut [u8], mut i: usize, field: u32, v: u64) -> Option<usize> {
    if v == 0 {
        return Some(i);
    }
    i = put_varint(out, i, ((field << 3) | 0) as u64)?;
    put_varint(out, i, v)
}

fn put_bool(out: &mut [u8], mut i: usize, field: u32, v: bool) -> Option<usize> {
    if !v {
        return Some(i);
    }
    i = put_varint(out, i, ((field << 3) | 0) as u64)?;
    put_varint(out, i, 1)
}

fn decode_string_field(
    buf: &[u8],
    i: &mut usize,
    out: &mut Option<String>,
) -> Result<(), DecodeError> {
    let slice = read_len_delimited(buf, i)?;
    let s = std::str::from_utf8(slice).map_err(|_| DecodeError::BadWire)?;
    *out = Some(s.to_string());
    Ok(())
}

pub fn encode_frame(method: u32, body: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    i = put_varint(out, i, method as u64)?;
    i = put_varint(out, i, body.len() as u64)?;
    if i + body.len() > out.len() {
        return None;
    }
    out[i..i + body.len()].copy_from_slice(body);
    Some(i + body.len())
}

pub fn decode_frame(buf: &[u8]) -> Result<(u32, &[u8]), DecodeError> {
    let mut i = 0usize;
    let method = read_varint(buf, &mut i)? as u32;
    let n = read_varint(buf, &mut i)? as usize;
    let end = i.checked_add(n).ok_or(DecodeError::Truncated)?;
    if end > buf.len() {
        return Err(DecodeError::Truncated);
    }
    Ok((method, &buf[i..end]))
}

pub fn encode_list_profiles_resp(resp: &ListProfilesResp, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    for p in &resp.profiles {
        let mut nested = [0u8; 256];
        let mut ni = 0usize;
        ni = put_string(&mut nested, ni, 1, &p.profile_id)?;
        ni = put_u32(&mut nested, ni, 2, p.path)?;
        ni = put_string(&mut nested, ni, 3, &p.bin_path)?;
        i = put_varint(out, i, (1u64 << 3) | 2)?;
        i = put_varint(out, i, ni as u64)?;
        if i + ni > out.len() {
            return None;
        }
        out[i..i + ni].copy_from_slice(&nested[..ni]);
        i += ni;
    }
    Some(i)
}

pub fn decode_list_profiles_resp(buf: &[u8]) -> Result<ListProfilesResp, DecodeError> {
    let mut profiles = Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        if field == 1 && wt == 2 {
            let nested = read_len_delimited(buf, &mut i)?;
            let mut ni = 0usize;
            let mut profile_id = None;
            let mut path = 0u32;
            let mut bin_path = None;
            while ni < nested.len() {
                let t = read_varint(nested, &mut ni)? as u32;
                let f = t >> 3;
                let w = t & 7;
                match (f, w) {
                    (1, 2) => decode_string_field(nested, &mut ni, &mut profile_id)?,
                    (2, 0) => path = read_varint(nested, &mut ni)? as u32,
                    (3, 2) => decode_string_field(nested, &mut ni, &mut bin_path)?,
                    _ => skip_field(nested, &mut ni, w)?,
                }
            }
            profiles.push(ProfileInfo {
                profile_id: profile_id.unwrap_or_default(),
                path,
                bin_path: bin_path.unwrap_or_default(),
            });
        } else {
            skip_field(buf, &mut i, wt)?;
        }
    }
    Ok(ListProfilesResp { profiles })
}

pub fn encode_flash_profile_req(profile_id: &str, out: &mut [u8]) -> Option<usize> {
    put_string(out, 0, 1, profile_id)
}

pub fn decode_flash_profile_req(buf: &[u8]) -> Result<&[u8], DecodeError> {
    let mut i = 0usize;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        if field == 1 && wt == 2 {
            return read_len_delimited(buf, &mut i);
        }
        skip_field(buf, &mut i, wt)?;
    }
    Err(DecodeError::BadWire)
}

pub fn encode_flash_profile_resp(resp: &FlashProfileResp, out: &mut [u8]) -> Option<usize> {
    let mut i = put_u32(out, 0, 1, resp.status)?;
    i = put_string(out, i, 2, &resp.message)?;
    Some(i)
}

pub fn decode_flash_profile_resp(buf: &[u8]) -> Result<FlashProfileResp, DecodeError> {
    let mut i = 0usize;
    let mut status = 0u32;
    let mut message = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => status = read_varint(buf, &mut i)? as u32,
            (2, 2) => decode_string_field(buf, &mut i, &mut message)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(FlashProfileResp {
        status,
        message: message.unwrap_or_default(),
    })
}

pub fn encode_reboot_resp(resp: &RebootResp, out: &mut [u8]) -> Option<usize> {
    let mut i = put_u32(out, 0, 1, resp.status)?;
    i = put_string(out, i, 2, &resp.message)?;
    Some(i)
}

pub fn decode_reboot_resp(buf: &[u8]) -> Result<RebootResp, DecodeError> {
    let mut i = 0usize;
    let mut status = 0u32;
    let mut message = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => status = read_varint(buf, &mut i)? as u32,
            (2, 2) => decode_string_field(buf, &mut i, &mut message)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(RebootResp {
        status,
        message: message.unwrap_or_default(),
    })
}

pub fn encode_get_status_resp(resp: &GetStatusResp, out: &mut [u8]) -> Option<usize> {
    let mut i = put_u32(out, 0, 1, resp.state)?;
    i = put_string(out, i, 2, &resp.last_profile_id)?;
    i = put_bool(out, i, 3, resp.esp_link_ok)?;
    i = put_string(out, i, 4, &resp.message)?;
    Some(i)
}

pub fn decode_get_status_resp(buf: &[u8]) -> Result<GetStatusResp, DecodeError> {
    let mut i = 0usize;
    let mut state = 0u32;
    let mut last_profile_id = None;
    let mut esp_link_ok = false;
    let mut message = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => state = read_varint(buf, &mut i)? as u32,
            (2, 2) => decode_string_field(buf, &mut i, &mut last_profile_id)?,
            (3, 0) => esp_link_ok = read_varint(buf, &mut i)? != 0,
            (4, 2) => decode_string_field(buf, &mut i, &mut message)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(GetStatusResp {
        state,
        last_profile_id: last_profile_id.unwrap_or_default(),
        esp_link_ok,
        message: message.unwrap_or_default(),
    })
}

pub fn encode_submit_job_req(
    bench: &str,
    smoke: bool,
    formal: bool,
    profile_id: &str,
    out: &mut [u8],
) -> Option<usize> {
    let mut i = put_string(out, 0, 1, bench)?;
    i = put_bool(out, i, 2, smoke)?;
    i = put_bool(out, i, 3, formal)?;
    i = put_string(out, i, 4, profile_id)?;
    Some(i)
}

pub fn decode_submit_job_req(buf: &[u8]) -> Result<(String, bool, bool, String), DecodeError> {
    let mut i = 0usize;
    let mut bench = None;
    let mut smoke = false;
    let mut formal = false;
    let mut profile_id = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 2) => decode_string_field(buf, &mut i, &mut bench)?,
            (2, 0) => smoke = read_varint(buf, &mut i)? != 0,
            (3, 0) => formal = read_varint(buf, &mut i)? != 0,
            (4, 2) => decode_string_field(buf, &mut i, &mut profile_id)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok((
        bench.unwrap_or_default(),
        smoke,
        formal,
        profile_id.unwrap_or_default(),
    ))
}

pub fn encode_submit_job_resp(resp: &SubmitJobResp, out: &mut [u8]) -> Option<usize> {
    let mut i = put_u32(out, 0, 1, resp.status)?;
    i = put_u64(out, i, 2, resp.job_id)?;
    i = put_string(out, i, 3, &resp.message)?;
    Some(i)
}

pub fn decode_submit_job_resp(buf: &[u8]) -> Result<SubmitJobResp, DecodeError> {
    let mut i = 0usize;
    let mut status = 0u32;
    let mut job_id = 0u64;
    let mut message = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => status = read_varint(buf, &mut i)? as u32,
            (2, 0) => job_id = read_varint(buf, &mut i)?,
            (3, 2) => decode_string_field(buf, &mut i, &mut message)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(SubmitJobResp {
        status,
        job_id,
        message: message.unwrap_or_default(),
    })
}

pub fn encode_get_job_req(job_id: u64, out: &mut [u8]) -> Option<usize> {
    put_u64(out, 0, 1, job_id)
}

pub fn decode_get_job_req(buf: &[u8]) -> Result<u64, DecodeError> {
    let mut i = 0usize;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        if field == 1 && wt == 0 {
            return Ok(read_varint(buf, &mut i)?);
        }
        skip_field(buf, &mut i, wt)?;
    }
    Err(DecodeError::BadWire)
}

pub fn encode_get_job_resp(resp: &GetJobResp, out: &mut [u8]) -> Option<usize> {
    let mut i = put_u32(out, 0, 1, resp.status)?;
    i = put_u32(out, i, 2, resp.job_state)?;
    i = put_string(out, i, 3, &resp.message)?;
    i = put_string(out, i, 4, &resp.bench)?;
    i = put_string(out, i, 5, &resp.profile_id)?;
    Some(i)
}

pub fn decode_get_job_resp(buf: &[u8]) -> Result<GetJobResp, DecodeError> {
    let mut i = 0usize;
    let mut status = 0u32;
    let mut job_state = 0u32;
    let mut message = None;
    let mut bench = None;
    let mut profile_id = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => status = read_varint(buf, &mut i)? as u32,
            (2, 0) => job_state = read_varint(buf, &mut i)? as u32,
            (3, 2) => decode_string_field(buf, &mut i, &mut message)?,
            (4, 2) => decode_string_field(buf, &mut i, &mut bench)?,
            (5, 2) => decode_string_field(buf, &mut i, &mut profile_id)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(GetJobResp {
        status,
        job_state,
        message: message.unwrap_or_default(),
        bench: bench.unwrap_or_default(),
        profile_id: profile_id.unwrap_or_default(),
    })
}

pub fn encode_cancel_job_req(job_id: u64, out: &mut [u8]) -> Option<usize> {
    put_u64(out, 0, 1, job_id)
}

pub fn decode_cancel_job_req(buf: &[u8]) -> Result<u64, DecodeError> {
    decode_get_job_req(buf)
}

pub fn encode_cancel_job_resp(resp: &CancelJobResp, out: &mut [u8]) -> Option<usize> {
    let mut i = put_u32(out, 0, 1, resp.status)?;
    i = put_string(out, i, 2, &resp.message)?;
    Some(i)
}

pub fn decode_cancel_job_resp(buf: &[u8]) -> Result<CancelJobResp, DecodeError> {
    let mut i = 0usize;
    let mut status = 0u32;
    let mut message = None;
    while i < buf.len() {
        let tag = read_varint(buf, &mut i)? as u32;
        let field = tag >> 3;
        let wt = tag & 7;
        match (field, wt) {
            (1, 0) => status = read_varint(buf, &mut i)? as u32,
            (2, 2) => decode_string_field(buf, &mut i, &mut message)?,
            _ => skip_field(buf, &mut i, wt)?,
        }
    }
    Ok(CancelJobResp {
        status,
        message: message.unwrap_or_default(),
    })
}
