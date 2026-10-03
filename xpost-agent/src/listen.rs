//! TCP RPC for TR3000 / fwctl: fixed 8-byte header (method LE + len LE) + body.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// Don't reprint noisy COUNTERS / `rxcnt` lines more often than this.
const NOISY_RPC_LOG_EVERY: Duration = Duration::from_secs(2);

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
    /// Optional argv relative to workspace root, run before flash (e.g. cargo xtask … build).
    #[serde(default)]
    build: Option<Vec<String>>,
}

struct State {
    session: Session<FileSink>,
    profiles: Vec<ProfileEntry>,
    root: PathBuf,
    /// Last time we logged a noisy COUNTERS / rxcnt RPC (throttle).
    last_noisy_log: Option<Instant>,
}

fn ts() -> String {
    let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return "??:??:??".into();
    };
    let secs = dur.as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}Z")
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}.{:01}s", s, d.subsec_millis() / 100)
    }
}

fn fmt_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1}MiB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1}KiB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

fn method_name(m: u32) -> &'static str {
    match m {
        AGENT_FLASH_PROFILE => "FlashProfile",
        AGENT_GET_STATUS => "GetStatus",
        AGENT_PROGRAM => "Program",
        AGENT_CLEAR => "Clear",
        AGENT_COUNTERS => "Counters",
        AGENT_PROBE => "Probe",
        AGENT_PROBE_BURST => "ProbeBurst",
        AGENT_REQUEST => "Request",
        _ => "Unknown",
    }
}

fn peer_label(addr: Option<SocketAddr>) -> String {
    addr.map(|a| a.to_string())
        .unwrap_or_else(|| "?".into())
}

fn log_line(peer: &str, msg: &str) {
    eprintln!("[{}] [{peer}] {msg}", ts());
}

fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

fn is_noisy_rpc(method: u32, body: &[u8]) -> bool {
    match method {
        AGENT_COUNTERS => true,
        AGENT_REQUEST => {
            let Ok(cmd) = std::str::from_utf8(body) else {
                return false;
            };
            let cmd = cmd.trim();
            cmd.starts_with("rxcnt") || cmd.eq_ignore_ascii_case("counters")
        }
        _ => false,
    }
}

pub fn serve(addr: &str, port: Option<&Path>, log: &Path) -> Result<()> {
    let sink = FileSink::create(log, true)
        .with_context(|| format!("create serial log {}", log.display()))?;
    eprintln!("[{}] xpost-agent serial log {}", ts(), sink.path().display());
    let session = Session::open(port, Chip::Esp32c3, sink).context("open session")?;
    let root = workspace_root();
    let profiles = load_profiles(&root)?;
    eprintln!(
        "[{}] xpost-agent profiles={} from {}",
        ts(),
        profiles.len(),
        root.join("profiles.toml").display()
    );
    for p in &profiles {
        let xdp = match (&p.xdp_name, &p.xdp_elf) {
            (Some(n), Some(e)) => format!(" xdp={n} ({e})"),
            (None, Some(e)) => format!(" xdp_elf={e}"),
            _ => String::new(),
        };
        let build = p
            .build
            .as_ref()
            .map(|a| format!(" build=[{}]", a.join(" ")))
            .unwrap_or_default();
        eprintln!("[{}]   • {} → {}{xdp}{build}", ts(), p.id, p.bin);
    }
    let state = Arc::new(Mutex::new(State {
        session,
        profiles,
        root,
        last_noisy_log: None,
    }));
    let listener = TcpListener::bind(addr).with_context(|| format!("bind {addr}"))?;
    eprintln!(
        "[{}] xpost-agent listen {addr} (GetStatus silent; rxcnt/counters ≤1/{:?})",
        ts(),
        NOISY_RPC_LOG_EVERY
    );
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let state = Arc::clone(&state);
                if let Err(e) = handle_conn(stream, &state) {
                    eprintln!("[{}] xpost-agent rpc: {e:#}", ts());
                }
            }
            Err(e) => eprintln!("[{}] xpost-agent accept: {e}", ts()),
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
    let peer = peer_label(stream.peer_addr().ok());
    // Build + flash can take many minutes on a cold toolchain.
    stream
        .set_read_timeout(Some(Duration::from_secs(1200)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_secs(1200)))
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

    // Polling path from TR3000 wait_idle — stay silent.
    if method == AGENT_GET_STATUS {
        let mut pb = [0u8; 64];
        let n = encode_get_status_pb(AGENT_IDLE, true, "ok", &mut pb).unwrap_or(0);
        return write_raw(&mut stream, method, &pb[..n]);
    }

    let t0 = Instant::now();
    let name = method_name(method);
    let summary = rpc_body_summary(method, &body);

    if method == AGENT_FLASH_PROFILE {
        log_line(&peer, &format!("→ {name} {summary}"));
        let (status, msg) = match flash_profile(&body, state, &peer) {
            Ok(m) => (STATUS_OK, m),
            Err(e) => {
                log_line(
                    &peer,
                    &format!("✗ {name} failed in {}: {e:#}", fmt_dur(t0.elapsed())),
                );
                (STATUS_BAD, format!("{e:#}"))
            }
        };
        if status == STATUS_OK {
            log_line(
                &peer,
                &format!("← {name} ok in {} — {msg}", fmt_dur(t0.elapsed())),
            );
        }
        let mut pb = [0u8; 512];
        let n = encode_flash_profile_pb(status, &msg, &mut pb).unwrap_or(0);
        return write_raw(&mut stream, method, &pb[..n]);
    }

    let noisy = is_noisy_rpc(method, &body);
    let log_this = if noisy {
        let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        let due = g
            .last_noisy_log
            .map(|t| t.elapsed() >= NOISY_RPC_LOG_EVERY)
            .unwrap_or(true);
        if due {
            g.last_noisy_log = Some(Instant::now());
        }
        due
    } else {
        true
    };
    if log_this {
        log_line(&peer, &format!("→ {name} {summary}"));
    }

    let resp = dispatch(method, &body, state);
    let (status, text) = match resp {
        Ok(s) => (STATUS_OK, s),
        Err(e) => (STATUS_BAD, format!("{e:#}")),
    };
    if log_this || status != STATUS_OK {
        let preview = truncate(&text, 120);
        let mark = if status == STATUS_OK { "←" } else { "✗" };
        log_line(
            &peer,
            &format!(
                "{mark} {name} status={status} in {} — {preview}",
                fmt_dur(t0.elapsed())
            ),
        );
    }
    write_resp(&mut stream, method, status, &text)
}

fn rpc_body_summary(method: u32, body: &[u8]) -> String {
    match method {
        AGENT_FLASH_PROFILE => decode_flash_profile_id(body)
            .map(|id| format!("profile=`{id}`"))
            .unwrap_or_else(|_| format!("body={}B", body.len())),
        AGENT_PROGRAM => std::str::from_utf8(body)
            .map(|p| format!("elf={}", p.trim()))
            .unwrap_or_else(|_| format!("body={}B", body.len())),
        AGENT_REQUEST => std::str::from_utf8(body)
            .map(|c| format!("`{}`", c.trim()))
            .unwrap_or_else(|_| format!("body={}B", body.len())),
        AGENT_PROBE | AGENT_PROBE_BURST => std::str::from_utf8(body)
            .map(|t| t.trim().to_string())
            .unwrap_or_else(|_| format!("body={}B", body.len())),
        AGENT_CLEAR | AGENT_COUNTERS => String::new(),
        _ => format!("body={}B", body.len()),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.replace('\n', "\\n");
    if s.chars().count() <= max {
        s
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
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

fn run_profile_build(root: &Path, entry: &ProfileEntry, peer: &str) -> Result<Duration> {
    let Some(argv) = entry.build.as_ref() else {
        log_line(peer, &format!("build profile `{}`: (none)", entry.id));
        return Ok(Duration::ZERO);
    };
    if argv.is_empty() {
        return Ok(Duration::ZERO);
    }
    let (prog, args) = argv.split_first().unwrap();
    log_line(
        peer,
        &format!(
            "build profile `{}`: {} {}  (cwd {})",
            entry.id,
            prog,
            args.join(" "),
            root.display()
        ),
    );
    let t0 = Instant::now();
    let status = Command::new(prog)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("spawn build for profile `{}`", entry.id))?;
    let elapsed = t0.elapsed();
    if !status.success() {
        bail!(
            "build failed for profile `{}` in {} (status={status}): {} {}",
            entry.id,
            fmt_dur(elapsed),
            prog,
            args.join(" ")
        );
    }
    log_line(
        peer,
        &format!("build profile `{}` ok in {}", entry.id, fmt_dur(elapsed)),
    );
    Ok(elapsed)
}

fn flash_profile(body: &[u8], state: &Mutex<State>, peer: &str) -> Result<String> {
    let id = decode_flash_profile_id(body)?;
    let (root, entry) = {
        let g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
        let entry = g
            .profiles
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .with_context(|| format!("unknown profile `{id}`"))?;
        (g.root.clone(), entry)
    };

    // Compile outside the session lock so build logs are visible and USB stays free until flash.
    let build_dur = run_profile_build(&root, &entry, peer)?;

    let firmware = root.join(&entry.bin);
    let partitions = root.join(&entry.partition_table);
    if !firmware.is_file() {
        bail!(
            "firmware not found after build: {} (profile `{id}`)",
            firmware.display()
        );
    }
    if !partitions.is_file() {
        bail!("partitions not found: {}", partitions.display());
    }
    let fw_sz = file_size(&firmware).unwrap_or(0);
    let xdp_path = entry.xdp_elf.as_ref().map(|p| root.join(p));
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
    let xdp_sz = xdp_path
        .as_ref()
        .and_then(|p| file_size(p))
        .unwrap_or(0);

    log_line(
        peer,
        &format!(
            "[fw] profile `{id}`: {} ({})  [part] {}",
            firmware.display(),
            fmt_bytes(fw_sz),
            partitions.display(),
        ),
    );
    if let (Some(n), Some(p)) = (&xdp_name_owned, &xdp_path) {
        log_line(
            peer,
            &format!(
                "[xdp] profile `{id}`: name={n} {} ({})",
                p.display(),
                fmt_bytes(xdp_sz)
            ),
        );
    } else {
        log_line(peer, &format!("[xdp] profile `{id}`: (none)"));
    }

    let mut g = state.lock().map_err(|_| anyhow::anyhow!("session lock"))?;
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
    let t_flash = Instant::now();
    log_line(
        peer,
        &format!(
            "[fw]+[xdp] profile `{id}`: USB program (cold_table={:#x}+{:#x}) — watch [fw]/[part]/[xdp]/[reset]/[uart]/[cmd] below",
            JUST_DROP_TABLE.base, JUST_DROP_TABLE.len
        ),
    );
    g.session
        .program(&img)
        .with_context(|| format!("program profile `{id}`"))?;
    let flash_dur = t_flash.elapsed();
    log_line(
        peer,
        &format!(
            "[reset] profile `{id}`: program+reset done in {} (build {})",
            fmt_dur(flash_dur),
            fmt_dur(build_dur)
        ),
    );
    Ok(format!(
        "flashed profile={id} fw={} build={} flash={}",
        fmt_bytes(fw_sz),
        fmt_dur(build_dur),
        fmt_dur(flash_dur)
    ))
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
            let sz = file_size(&path).unwrap_or(0);
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
                    format!(
                        "overwritten name={name} offset={offset:#x} size={}",
                        fmt_bytes(sz)
                    )
                }
                xpost_serial::InstallOutcome::Created { offset } => {
                    format!(
                        "created name={name} offset={offset:#x} size={}",
                        fmt_bytes(sz)
                    )
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
