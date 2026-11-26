//! Shared USB device and deployment configuration.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[cfg(not(feature = "test-device"))]
use anyhow::Context;
use anyhow::Result;
use std::time::SystemTime;
use xpost_serial::client::flash::XdpTable;
use xpost_serial::client::ops::FlashOp;
use xpost_serial::client::{Interrupted, ParseError, RequestTimeout};
use xpost_serial::reply::{ErrReply, MapError, PerfError, ServerError, SyntaxError};
#[cfg(not(feature = "test-device"))]
use xpost_serial::{Chip, FlashConfig, OpenOptions, SerialHandler};
use xpost_serial::{FlashResult, command::Command, reply::OkReply};

use crate::profiles::Profiles;
#[cfg(not(feature = "test-device"))]
use crate::util::{eprint_line, ts};

/// Don't reprint noisy get-map / get-perf lines more often than this.
pub const NOISY_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Default board partitions.csv used for FlashConfig XDP span (all examples share
/// `xdp @ 0x310000`). Used when the registry has no fw entries yet.
#[cfg(not(feature = "test-device"))]
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
                PerfError::Epoch => f.write_str("ERR perf epoch"),
                PerfError::Offset => f.write_str("ERR perf offset"),
                PerfError::Phase => f.write_str("ERR perf phase"),
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
    pub handler: Device,
    pub profiles: Profiles,
    pub root: PathBuf,
    pub last_noisy_log: Option<Instant>,
}

pub type SharedState = Arc<Mutex<AgentState>>;

pub fn open(root: PathBuf, profiles: Profiles, log_dir: &Path) -> Result<SharedState> {
    #[cfg(feature = "test-device")]
    let handler = Device::Fixture(crate::fixture::Fixture::open()?);
    #[cfg(not(feature = "test-device"))]
    let handler = Device::Serial(open_serial(&root, &profiles, log_dir)?);
    #[cfg(feature = "test-device")]
    let _ = log_dir;
    Ok(Arc::new(Mutex::new(AgentState {
        handler,
        profiles,
        root,
        last_noisy_log: None,
    })))
}

#[cfg(not(feature = "test-device"))]
fn open_serial(root: &Path, profiles: &Profiles, log_dir: &Path) -> Result<SerialHandler> {
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

    eprint_line(&format!(
        "[{}] xpost-agent profiles fw={} xdp={} empty={}",
        ts(),
        profiles.fw_count(),
        profiles.xdp_count(),
        profiles.is_empty()
    ));
    let info = handler.board_info().context("espflash Board Info")?;
    eprint_line(&format!("[{}] Board Info {info:#?}", ts()));
    Ok(handler)
}

pub fn load_profiles(root: &Path) -> Result<Profiles> {
    let path = std::env::var("XPOST_PROFILES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("profiles.toml"));
    Profiles::load_path(&path)
}

pub enum Device {
    #[cfg(not(feature = "test-device"))]
    Serial(SerialHandler),
    #[cfg(feature = "test-device")]
    Fixture(crate::fixture::Fixture),
}

impl Device {
    pub fn request(&self, cmd: &Command<'_>) -> Result<OkReply<'_>, AgentError> {
        self.request_at(cmd).map(|(reply, _)| reply)
    }

    pub fn request_at(&self, cmd: &Command<'_>) -> Result<(OkReply<'_>, SystemTime), AgentError> {
        match self {
            #[cfg(not(feature = "test-device"))]
            Self::Serial(handler) => handler.request_at(cmd),
            #[cfg(feature = "test-device")]
            Self::Fixture(fixture) => fixture
                .request(cmd)
                .map(|reply| (reply, SystemTime::now()))
                .map_err(Into::into),
        }
    }

    pub fn flash(&self, op: FlashOp) -> io::Result<FlashResult> {
        self.flash_with_table(op, None)
    }

    pub fn flash_with_table(
        &self,
        op: FlashOp,
        table: Option<XdpTable>,
    ) -> io::Result<FlashResult> {
        match self {
            #[cfg(not(feature = "test-device"))]
            Self::Serial(handler) => handler.flash_with_table(op, table),
            #[cfg(feature = "test-device")]
            Self::Fixture(fixture) => {
                let _ = table;
                fixture.flash(op)
            }
        }
    }

    pub fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> io::Result<Vec<String>> {
        match self {
            #[cfg(not(feature = "test-device"))]
            Self::Serial(handler) => handler.lines_in_range(since, until),
            #[cfg(feature = "test-device")]
            Self::Fixture(_) => {
                let _ = (since, until);
                Ok(Vec::new())
            }
        }
    }

    pub fn shutdown(self) {
        match self {
            #[cfg(not(feature = "test-device"))]
            Self::Serial(handler) => handler.shutdown(),
            #[cfg(feature = "test-device")]
            Self::Fixture(_) => {}
        }
    }
}
