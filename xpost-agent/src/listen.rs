//! TCP RPC for TR3000 / fwctl: fixed 8-byte header (method LE + len LE) + body.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use xpost_serial::{
    Chip, FileSink, Image, JUST_DROP_TABLE, Session, XdpImage, xdp_name_from_file,
};

use crate::{probe_ack, probe_burst};

pub const AGENT_FLASH_PROFILE: u32 = 2;
pub const AGENT_GET_STATUS: u32 = 4;
pub const AGENT_PROGRAM: u32 = 10;
pub const AGENT_CLEAR: u32 = 11;
pub const AGENT_COUNTERS: u32 = 12;
pub const AGENT_PROBE: u32 = 13;
/// Fire-and-forget N STIMs. Body: `esp:port:count`. Reply: `sent=N`.
pub const AGENT_PROBE_BURST: u32 = 14;
/// One App-mode serial line (body = command text). Reply = first OK/ERR line.
pub const AGENT_REQUEST: u32 = 15;

pub const AGENT_IDLE: u32 = 1;
pub const STATUS_OK: u32 = 200;
pub const STATUS_BAD: u32 = 400;

const MAX_BODY: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct ProfilesFile {
    #[serde(default, rename = "profile")]
    profiles: Vec<ProfileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileEntry {
    id: String,
    bin: String,
    partition_table: String,
    #[serde(default)]
    xdp_elf: Option<String>,
    #[serde(default)]
    xdp_name: Option<String>,
}

struct State {
    session: Session<FileSink>,
    profiles: Vec<ProfileEntry>,
    root: PathBuf,
}

pub fn serve(addr: &str, port: Option<&Path>, log: &Path) -> Result<()> {
    let sink = FileSink::create(log, true)
        .with_context(|| format!("create serial log {}", log.display()))?;
    eprintln!("xpost-agent serial log {}", sink.path().display());
    let session = Session::open(port, Chip::Esp32c3, sink).context("open session")?;
    let root = workspace_root();
    let profiles = load_profiles(&root)?;
    eprintln!(
        "xpost-agent profiles={} from {}",
        profiles.len(),
        root.join("profiles.toml").display()
    );
    let state = Arc::new(Mutex::new(State {
        session,
        profiles,
        root,
    }));
    let listener = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    eprintln!("xpost-agent listen {addr}");
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let state = Arc::clone(&state);
                if let Err(e) = handle_conn(stream, &state) {
                    eprintln!("xpost-agent rpc: {e:#}");
                }
            }
            Err(e) => eprintln!("xpost-agent accept: {e}"),
        }
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    if let Ok(p) = std::env::var("XPOST_ROOT") {
        return PathBuf::from(p);
    }
    // Prefer cwd when started from xpost/; else walk up looking for profiles.toml.
    let mut cur = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..6 {
        if cur.join("profiles.toml").is_file() {
            return cur;
        }
        if !cur.pop() {
            break;
        }
    }
    PathBuf::from(".")
}

fn load_profiles(root: &Path) -> Result<Vec<ProfileEntry>> {
    let path = std::env::var("XPOST_PROFILES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("profiles.toml"));
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read profiles {}", path.display()))?;
    let file: ProfilesFile = toml::from_str(&text)
        .with_context(|| format!("parse profiles {}", path.display()))?;
    Ok(file.profiles)
}

fn handle_conn(mut stream: TcpStream, state: &Mutex<State>) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(300)))
        .ok();
    let mut hdr = [0u8; 8];
    stream
        .read_exact(&mut hdr)
        .context("read rpc header")?;
    let method = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    let len = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    if len > MAX_BODY {
        bail!("body too large: {len}");
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut body).context("read rpc body")?;
    }

    if method == AGENT_GET_STATUS {
        let mut pb = [0u8; 64];
        let n = encode_get_status_pb(AGENT_IDLE, true, "ok", &mut pb).unwrap_or(0);
        return write_raw(&mut stream, method, &pb[..n]);
    }

    if method == AGENT_FLASH_PROFILE {
        let (status, msg) = match flash_profile(&body, state) {
            Ok(m) => (STATUS_OK, m),
            Err(e) => (STATUS_BAD, format!("{e:#}")),
        };
        let mut pb = [0u8; 512];
        let n = encode_flash_profile_pb(status, &msg, &mut pb).unwrap_or(0);
        return write_raw(&mut stream, method, &pb[..n]);
    }

    let resp = dispatch(method, &body, state);
    let (status, text) = match resp {
        Ok(s) => (STATUS_OK, s),
        Err(e) => (STATUS_BAD, format!("{e:#}")),
    };
    write_resp(&mut stream, method, status, &text)
}

fn write_raw(stream: &mut TcpStream, method: u32, body: &[u8]) -> Result<()> {
    let mut hdr = [0u8; 8];
    hdr[0..4].copy_from_slice(&method.to_le_bytes());
    hdr[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
    stream.write_all(&hdr)?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

/// Minimal protobuf: field1=state, field3=esp_link_ok, field4=message.
fn encode_get_status_pb(state: u32, link_ok: bool, msg: &str, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    out.get_mut(i).map(|b| *b = (1 << 3) | 0)?;
    i += 1;
    out.get_mut(i).map(|b| *b = state as u8)?;
    i += 1;
    if link_ok {
        out.get_mut(i).map(|b| *b = (3 << 3) | 0)?;
        i += 1;
        out.get_mut(i).map(|b| *b = 1)?;
        i += 1;
    }
    if !msg.is_empty() {
        out.get_mut(i).map(|b| *b = (4 << 3) | 2)?;
        i += 1;
        out.get_mut(i).map(|b| *b = msg.len() as u8)?;
        i += 1;
        if i + msg.len() > out.len() {
            return None;
        }
        out[i..i + msg.len()].copy_from_slice(msg.as_bytes());
        i += msg.len();
    }
    Some(i)
}

fn put_varint(out: &mut [u8], mut i: usize, mut v: u32) -> Option<usize> {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        *out.get_mut(i)? = b;
        i += 1;
        if v == 0 {
            return Some(i);
        }
    }
}

/// FlashProfileResp: field1 status varint, field2 message string.
fn encode_flash_profile_pb(status: u32, msg: &str, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    *out.get_mut(i)? = (1 << 3) | 0;
    i += 1;
    i = put_varint(out, i, status)?;
    let bytes = msg.as_bytes();
    if bytes.len() > 200 {
        return None;
    }
    *out.get_mut(i)? = (2 << 3) | 2;
    i += 1;
    *out.get_mut(i)? = bytes.len() as u8;
    i += 1;
    if i + bytes.len() > out.len() {
        return None;
    }
    out[i..i + bytes.len()].copy_from_slice(bytes);
    i += bytes.len();
    Some(i)
}

fn write_resp(stream: &mut TcpStream, method: u32, status: u32, text: &str) -> Result<()> {
    let mut body = Vec::with_capacity(4 + text.len());
    body.extend_from_slice(&status.to_le_bytes());
    body.extend_from_slice(text.as_bytes());
    write_raw(stream, method, &body)
}

fn decode_flash_profile_id(body: &[u8]) -> Result<&str> {
    // protobuf field 1 string
    let mut i = 0usize;
    while i < body.len() {
        let tag = body[i];
        i += 1;
        let field = tag >> 3;
        let wt = tag & 7;
        if field == 1 && wt == 2 {
            if i >= body.len() {
                bail!("truncated flash profile id");
            }
            let n = body[i] as usize;
            i += 1;
            if i + n > body.len() {
                bail!("truncated flash profile id");
            }
            return std::str::from_utf8(&body[i..i + n]).context("profile id utf8");
        }
        // skip other fields lightly
        match wt {
            0 => {
                while i < body.len() && body[i] & 0x80 != 0 {
                    i += 1;
                }
                i += 1;
            }
            2 => {
                if i >= body.len() {
                    bail!("bad protobuf");
                }
                let n = body[i] as usize;
                i += 1 + n;
            }
            _ => bail!("unsupported protobuf wire type {wt}"),
        }
    }
    bail!("missing profile_id")
}

fn flash_profile(body: &[u8], state: &Mutex<State>) -> Result<String> {
    let id = decode_flash_profile_id(body)?;
    let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
    let entry = g
        .profiles
        .iter()
        .find(|p| p.id == id)
        .cloned()
        .with_context(|| format!("unknown profile `{id}`"))?;
    let firmware = g.root.join(&entry.bin);
    let partitions = g.root.join(&entry.partition_table);
    if !firmware.is_file() {
        bail!("firmware not found: {}", firmware.display());
    }
    if !partitions.is_file() {
        bail!("partitions not found: {}", partitions.display());
    }
    let xdp_path = entry.xdp_elf.as_ref().map(|p| g.root.join(p));
    if let Some(ref xp) = xdp_path {
        if !xp.is_file() {
            bail!("xdp elf not found: {}", xp.display());
        }
    }
    let xdp_name_owned = match (&entry.xdp_name, &xdp_path) {
        (Some(n), _) => Some(n.clone()),
        (None, Some(xp)) => Some(
            xdp_name_from_file(xp)
                .with_context(|| format!("xdp name from {}", xp.display()))?,
        ),
        _ => None,
    };
    g.session.cold_table = JUST_DROP_TABLE;
    g.session.image_capacity = JUST_DROP_TABLE.image_capacity;
    let xdp = match (&xdp_name_owned, &xdp_path) {
        (Some(name), Some(path)) => Some(XdpImage {
            name: name.as_str(),
            path: path.as_path(),
        }),
        _ => None,
    };
    let img = Image {
        firmware: firmware.as_path(),
        partitions: partitions.as_path(),
        xdp,
    };
    eprintln!("xpost-agent FlashProfile `{id}` → {}", firmware.display());
    g.session
        .program(&img)
        .with_context(|| format!("program profile `{id}`"))?;
    Ok(format!("flashed profile={id}"))
}

fn dispatch(method: u32, body: &[u8], state: &Mutex<State>) -> Result<String> {
    match method {
        AGENT_GET_STATUS => Ok(format!("state={AGENT_IDLE}")),
        AGENT_PROGRAM => {
            let path = std::str::from_utf8(body).context("program path utf8")?;
            let path = PathBuf::from(path.trim());
            if !path.is_file() {
                bail!("elf not found: {}", path.display());
            }
            let name = xdp_name_from_file(&path)
                .with_context(|| format!("parse name from {}", path.display()))?;
            let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
            g.session.cold_table = JUST_DROP_TABLE;
            g.session.image_capacity = JUST_DROP_TABLE.image_capacity;
            let outcome = g
                .session
                .install_xdp(&name, &path)
                .with_context(|| format!("install_xdp `{name}`"))?;
            g.session
                .reset_to_app()
                .context("reset_to_app after program")?;
            Ok(match outcome {
                xpost_serial::InstallOutcome::Overwritten { offset } => {
                    format!("overwritten name={name} offset={offset:#x}")
                }
                xpost_serial::InstallOutcome::Created { offset } => {
                    format!("created name={name} offset={offset:#x}")
                }
            })
        }
        AGENT_CLEAR => {
            let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
            g.session.cold_table = JUST_DROP_TABLE;
            g.session.clear_xdp().context("clear_xdp")?;
            g.session.reset_to_app().context("reset_to_app after clear")?;
            Ok("cleared".into())
        }
        AGENT_COUNTERS => {
            let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
            g.session
                .request_timeout("counters", Duration::from_secs(5))
                .context("request counters")
        }
        AGENT_PROBE => {
            let text = std::str::from_utf8(body).context("probe body utf8")?;
            let (esp, port) = parse_probe(text)?;
            let got = probe_ack(&esp, port, Duration::from_secs(2))?;
            Ok(got.into())
        }
        AGENT_PROBE_BURST => {
            let text = std::str::from_utf8(body).context("burst body utf8")?;
            let (esp, port, n) = parse_burst(text)?;
            let sent = probe_burst(&esp, port, n)?;
            Ok(format!("sent={sent}"))
        }
        AGENT_REQUEST => {
            let cmd = std::str::from_utf8(body).context("request body utf8")?;
            let cmd = cmd.trim();
            if cmd.is_empty() {
                bail!("empty request");
            }
            let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
            g.session
                .request_timeout(cmd, Duration::from_secs(5))
                .with_context(|| format!("request `{cmd}`"))
        }
        other => bail!("unknown method {other}"),
    }
}

fn parse_probe(text: &str) -> Result<(String, u16)> {
    let text = text.trim();
    if let Some((host, port)) = text.rsplit_once(':') {
        let port: u16 = port.parse().context("probe port")?;
        Ok((host.to_string(), port))
    } else {
        Ok((text.to_string(), 8080))
    }
}

fn parse_burst(text: &str) -> Result<(String, u16, u32)> {
    let text = text.trim();
    let mut parts = text.rsplitn(3, ':');
    let n: u32 = parts
        .next()
        .context("burst need esp:port:count")?
        .parse()
        .context("burst count")?;
    if n == 0 || n > 100_000 {
        bail!("burst count out of range: {n}");
    }
    let port: u16 = parts
        .next()
        .context("burst need esp:port:count")?
        .parse()
        .context("burst port")?;
    let esp = parts.next().context("burst need esp:port:count")?.to_string();
    Ok((esp, port, n))
}
