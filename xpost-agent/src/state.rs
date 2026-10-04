//! Shared USB [`SerialHandler`] + profiles for HTTP `/v1` and MCP `/mcp`.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use xpost_serial::client::{Command, Interrupted, ParseError, RequestTimeout};
use xpost_serial::reply::{ErrReply, MapError, PerfError, ServerError, SyntaxError};
use xpost_serial::{Chip, FlashConfig, OpenOptions, SerialHandler};

use crate::profiles::Profiles;
use crate::util::{eprint_line, ts};

/// Don't reprint noisy get-map / get-perf lines more often than this.
pub const NOISY_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Default board partitions.csv used for FlashConfig XDP span (all examples share
/// `xdp @ 0x310000`). Used when the registry has no fw entries yet.
const DEFAULT_PARTITIONS: &str = "examples/just_xdp/partitions.csv";

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
    pub profiles: Profiles,
    pub root: PathBuf,
    pub last_noisy_log: Option<Instant>,
}

pub type SharedState = Arc<Mutex<AgentState>>;

pub fn open(log_dir: &Path) -> Result<SharedState> {
    let root = workspace_root();
    let profiles = load_profiles(&root)?;
    let partitions_rel = profiles
        .default_partition_table()
        .unwrap_or(DEFAULT_PARTITIONS);
    let partitions = root.join(partitions_rel);
    let flash = FlashConfig::from_partitions_csv(&partitions)
        .with_context(|| format!("FlashConfig from {}", partitions.display()))?;
    let chip = profiles.default_chip()?.unwrap_or(Chip::Esp32c3);

    eprint_line(&format!(
        "[{}] xpost-agent serial log_dir {}",
        ts(),
        log_dir.display()
    ));
    let handler = SerialHandler::open(OpenOptions {
        port: None,
        chip,
        log_dir,
        tee_stdout: true,
        flash,
    })
    .context("open SerialHandler")?;

    handler
        .request::<AgentError>(&Command::GetInit)
        .map_err(|e| anyhow::anyhow!("GetInit: {e}"))?;

    eprint_line(&format!(
        "[{}] xpost-agent profiles fw={} xdp={} empty={} from {}",
        ts(),
        profiles.fw_count(),
        profiles.xdp_count(),
        profiles.is_empty(),
        root.join("profiles.toml").display()
    ));
    Ok(Arc::new(Mutex::new(AgentState {
        handler,
        profiles,
        root,
        last_noisy_log: None,
    })))
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

fn load_profiles(root: &Path) -> Result<Profiles> {
    let path = std::env::var("XPOST_PROFILES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("profiles.toml"));
    Profiles::load_path(&path)
}
