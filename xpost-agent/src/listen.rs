//! TCP RPC for TR3000 / fwctl: fixed 8-byte header (method LE + len LE) + body.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use xpost_serial::{
    Chip, FileSink, JUST_DROP_TABLE, Session, xdp_name_from_file,
};

use crate::{probe_ack, probe_burst};

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

struct State {
    session: Session<FileSink>,
}

pub fn serve(addr: &str, port: Option<&Path>, log: &Path) -> Result<()> {
    let sink = FileSink::create(log, true)
        .with_context(|| format!("create serial log {}", log.display()))?;
    eprintln!("xpost-agent serial log {}", sink.path().display());
    let session = Session::open(port, Chip::Esp32c3, sink).context("open session")?;
    let state = Arc::new(Mutex::new(State { session }));
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

fn handle_conn(mut stream: TcpStream, state: &Mutex<State>) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
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
    let resp = dispatch(method, &body, state);
    let (status, text) = match resp {
        Ok(s) => (STATUS_OK, s),
        Err(e) => (STATUS_BAD, format!("{e:#}")),
    };
    if method == AGENT_GET_STATUS {
        // Legacy protobuf GetStatusResp for wait_idle callers.
        let mut body = [0u8; 64];
        let n = encode_get_status_pb(AGENT_IDLE, status == STATUS_OK, &text, &mut body)
            .unwrap_or(0);
        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&method.to_le_bytes());
        hdr[4..8].copy_from_slice(&(n as u32).to_le_bytes());
        stream.write_all(&hdr)?;
        stream.write_all(&body[..n])?;
        stream.flush()?;
        return Ok(());
    }
    write_resp(&mut stream, method, status, &text)
}

/// Minimal protobuf: field1=state, field3=esp_link_ok, field4=message.
fn encode_get_status_pb(state: u32, link_ok: bool, msg: &str, out: &mut [u8]) -> Option<usize> {
    let mut i = 0usize;
    // field 1 varint
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

fn write_resp(stream: &mut TcpStream, method: u32, status: u32, text: &str) -> Result<()> {
    // Body: status u32 LE + UTF-8 message (same shape for all new methods).
    let mut body = Vec::with_capacity(4 + text.len());
    body.extend_from_slice(&status.to_le_bytes());
    body.extend_from_slice(text.as_bytes());
    let mut hdr = [0u8; 8];
    hdr[0..4].copy_from_slice(&method.to_le_bytes());
    hdr[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
    stream.write_all(&hdr)?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
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
            // Reboot so XdpRt::init rescans the empty cold table (Pass / no lease).
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
