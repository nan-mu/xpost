//! Shared USB [`SerialHandler`] + profiles for HTTP `/v1` and MCP `/mcp`.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Deserialize;
use xpost_serial::client::{Command, RequestTimeout};
use xpost_serial::client::error::{Interrupted, ParseError};
use xpost_serial::reply::{ErrReply, MapError, PerfError, ServerError, SyntaxError};
use xpost_serial::{Chip, FlashConfig, OpenOptions, SerialHandler};

use crate::util::{eprint_line, ts};

/// Don't reprint noisy get-map / get-perf lines more often than this.
pub const NOISY_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Default board partitions.csv used for FlashConfig XDP span (all examples share
/// `xdp @ 0x310000`).
const DEFAULT_PARTITIONS: &str = "examples/just_xdp/partitions.csv";

#[derive(Debug, Deserialize)]
struct ProfilesFile {
    #[serde(default, rename = "profile")]
    profiles: Vec<ProfileEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileEntry {
    pub id: String,
    pub bin: String,
    pub partition_table: String,
    #[serde(default)]
    pub xdp_elf: Option<String>,
    #[serde(default)]
    pub xdp_name: Option<String>,
    /// Optional argv relative to workspace root, run before flash.
    #[serde(default)]
    pub build: Option<Vec<String>>,
}

/// Concrete `E` for [`SerialHandler::request`].
pub enum AgentError {
    Parse(ParseError),
    Server(ServerError),
    Map(MapError),
    Perf(PerfError),
    Syntax(SyntaxError),
    Timeout(RequestTimeout),
    Interrupted,
    Io(io::Error),
}

impl ErrReply for AgentError {}

impl From<ParseError> for AgentError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}
impl From<ServerError> for AgentError {
    fn from(e: ServerError) -> Self {
        Self::Server(e)
    }
}
impl From<MapError> for AgentError {
    fn from(e: MapError) -> Self {
        Self::Map(e)
    }
}
impl From<PerfError> for AgentError {
    fn from(e: PerfError) -> Self {
        Self::Perf(e)
    }
}
impl From<SyntaxError> for AgentError {
    fn from(e: SyntaxError) -> Self {
        Self::Syntax(e)
    }
}
impl From<RequestTimeout> for AgentError {
    fn from(e: RequestTimeout) -> Self {
        Self::Timeout(e)
    }
}
impl From<Interrupted> for AgentError {
    fn from(_: Interrupted) -> Self {
        Self::Interrupted
    }
}
impl From<io::Error> for AgentError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Debug for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "parse: {e}"),
            Self::Server(e) => write!(f, "{e}"),
            Self::Map(e) => match e {
                MapError::NameNotFound => f.write_str("ERR map name-not-found"),
                MapError::KeyNotFound => f.write_str("ERR map key-not-found"),
                MapError::InvalidName => f.write_str("ERR map invalid-name"),
                MapError::InvalidKey => f.write_str("ERR map invalid-key"),
                MapError::InvalidValue => f.write_str("ERR map invalid-value"),
                MapError::Full => f.write_str("ERR map full"),
                MapError::Unsupported => f.write_str("ERR map unsupported"),
                MapError::BufferTooSmall => f.write_str("ERR map buffer-too-small"),
            },
            Self::Perf(e) => match e {
                PerfError::Unavailable => f.write_str("ERR perf unavailable"),
                PerfError::Unsupported => f.write_str("ERR perf unsupported"),
                PerfError::BufferTooSmall => f.write_str("ERR perf buffer-too-small"),
            },
            Self::Syntax(e) => match e {
                SyntaxError::BadArity => f.write_str("ERR bad-arity"),
                SyntaxError::InvalidUtf8 => f.write_str("ERR invalid-utf8"),
                SyntaxError::LineTooLong => f.write_str("ERR line-too-long"),
            },
            Self::Timeout(e) => write!(f, "{e}"),
            Self::Interrupted => f.write_str("interrupted by flash"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for AgentError {}

pub struct AgentState {
    pub handler: SerialHandler,
    pub profiles: Vec<ProfileEntry>,
    pub root: PathBuf,
    pub last_noisy_log: Option<Instant>,
}

pub type SharedState = Arc<Mutex<AgentState>>;

pub fn open(port: Option<&Path>, log: &Path) -> Result<SharedState> {
    let root = workspace_root();
    let partitions = root.join(DEFAULT_PARTITIONS);
    let flash = FlashConfig::from_partitions_csv(&partitions)
        .with_context(|| format!("FlashConfig from {}", partitions.display()))?;

    eprint_line(&format!(
        "[{}] xpost-agent serial log {}",
        ts(),
        log.display()
    ));
    let handler = SerialHandler::open(OpenOptions {
        port,
        chip: Chip::Esp32c3,
        log_path: log,
        tee_stdout: true,
        flash,
    })
    .context("open SerialHandler")?;

    handler
        .request::<AgentError>(&Command::GetInit)
        .map_err(|e| anyhow::anyhow!("GetInit: {e}"))?;

    let profiles = load_profiles(&root)?;
    eprint_line(&format!(
        "[{}] xpost-agent profiles={} from {}",
        ts(),
        profiles.len(),
        root.join("profiles.toml").display()
    ));
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
        eprint_line(&format!(
            "[{}]   • {} → {}{xdp}{build}",
            ts(),
            p.id,
            p.bin
        ));
    }
    Ok(Arc::new(Mutex::new(AgentState {
        handler,
        profiles,
        root,
        last_noisy_log: None,
    })))
}

/// Open a one-shot handler (CLI), including GetInit settle.
pub fn open_handler(port: Option<&Path>, log: &Path) -> Result<SerialHandler> {
    let root = workspace_root();
    let partitions = root.join(DEFAULT_PARTITIONS);
    let flash = FlashConfig::from_partitions_csv(&partitions)
        .with_context(|| format!("FlashConfig from {}", partitions.display()))?;
    eprint_line(&format!(
        "[{}] xpost-agent serial log {}",
        ts(),
        log.display()
    ));
    let handler = SerialHandler::open(OpenOptions {
        port,
        chip: Chip::Esp32c3,
        log_path: log,
        tee_stdout: true,
        flash,
    })
    .context("open SerialHandler")?;
    handler
        .request::<AgentError>(&Command::GetInit)
        .map_err(|e| anyhow::anyhow!("GetInit: {e}"))?;
    Ok(handler)
}

pub fn workspace_root() -> PathBuf {
    if let Ok(p) = std::env::var("XPOST_ROOT") {
        return PathBuf::from(p);
    }
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
