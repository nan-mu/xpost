//! Typed serial transcript events. Ordered by [`Event::seq`] like a file.

use std::fmt;
use std::path::Path;
use std::time::SystemTime;

/// Host-side session mode (UART exclusivity).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Closed,
    App,
    Download,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "closed"),
            Self::App => write!(f, "app"),
            Self::Download => write!(f, "download"),
        }
    }
}

/// Why a session transition happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionWhy {
    Open {
        port: String,
        baud: u32,
        chip: String,
    },
    Close,
    ResetToApp,
    ResetToDownload,
    ModeSwitch,
}

impl SessionWhy {
    pub fn label(&self) -> String {
        match self {
            Self::Open { port, baud, chip } => {
                format!("open {port} {baud} chip={chip}")
            }
            Self::Close => "close".into(),
            Self::ResetToApp => "reset → app".into(),
            Self::ResetToDownload => "reset → download".into(),
            Self::ModeSwitch => "mode_switch".into(),
        }
    }
}

/// Flash progress phase (write / readback / verified).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashPhase {
    Writing,
    Reading,
    Verified,
}

impl fmt::Display for FlashPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Writing => write!(f, "writing"),
            Self::Reading => write!(f, "reading"),
            Self::Verified => write!(f, "verified"),
        }
    }
}

/// What is being flashed / erased (drives transcript tags).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlashTarget {
    /// Application firmware ELF (via esp-idf image format).
    Firmware,
    /// Partition table at 0x8000.
    PartitionTable,
    /// Cold-region XDP ELF container.
    Xdp { name: String },
    /// Erase cold table span.
    ColdErase,
    /// Generic `write_bin` (unspecified payload).
    Bin,
}

impl FlashTarget {
    fn tag(&self) -> &'static str {
        match self {
            Self::Firmware => "fw",
            Self::PartitionTable => "part",
            Self::Xdp { .. } => "xdp",
            Self::ColdErase => "erase",
            Self::Bin => "bin",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Xdp { name } => format!(" name={name}"),
            _ => String::new(),
        }
    }
}

/// Host perspective: Tx = bytes written toward the MCU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Tx,
    Rx,
}

/// UART / command line role in the transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoRole {
    /// Host issued a serial App-mode command.
    CmdTx,
    /// MCU reply that completes an outstanding command (`OK ` / `ERR `).
    CmdRx,
    /// MCU-initiated line (boot / log), captured from UART — not a cmd reply.
    UartRx,
}

/// Line-oriented I/O payload (prototype: lines only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoData {
    Line(String),
}

/// Event kind for the serial transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Session {
        from: Mode,
        to: Mode,
        why: SessionWhy,
    },
    Flash {
        target: FlashTarget,
        phase: FlashPhase,
        addr: Option<u32>,
        len: u32,
        done: u32,
    },
    Io {
        role: IoRole,
        data: IoData,
    },
    Error {
        ctx: &'static str,
        msg: String,
    },
}

/// One ordered transcript record.
#[derive(Clone, Debug)]
pub struct Event {
    pub seq: u64,
    pub ts: SystemTime,
    pub kind: Kind,
}

impl Event {
    /// Render one transcript line (same shape as a DB view projection).
    pub fn transcript_line(&self) -> String {
        match &self.kind {
            Kind::Io {
                role: IoRole::CmdTx,
                data: IoData::Line(s),
            } => format!("[cmd] → {s}"),
            Kind::Io {
                role: IoRole::CmdRx,
                data: IoData::Line(s),
            } => format!("[cmd] ← {s}"),
            Kind::Io {
                role: IoRole::UartRx,
                data: IoData::Line(s),
            } => format!("[uart] {s}"),
            Kind::Session { why, from, to } => match why {
                SessionWhy::ResetToApp | SessionWhy::ResetToDownload => {
                    format!("[reset] {} ({} → {})", why.label(), from, to)
                }
                SessionWhy::Open { .. } | SessionWhy::Close | SessionWhy::ModeSwitch => {
                    format!("[session] {} ({} → {})", why.label(), from, to)
                }
            },
            Kind::Flash {
                target,
                phase,
                addr,
                len,
                done,
            } => {
                let addr = addr
                    .map(|a| format!("{a:#x}"))
                    .unwrap_or_else(|| "-".into());
                format!(
                    "[{}] {}{} addr={addr} {done}/{len}",
                    target.tag(),
                    phase,
                    target.detail()
                )
            }
            Kind::Error { ctx, msg } => format!("[err] {ctx}: {msg}"),
        }
    }
}

/// Firmware + optional cold-region XDP ELF for [`crate::Session::program`].
#[derive(Clone, Debug)]
pub struct Image<'a> {
    pub firmware: &'a std::path::Path,
    pub partitions: &'a std::path::Path,
    pub xdp: Option<XdpImage<'a>>,
}

/// XDP ELF install target (name must match the name inside the file).
#[derive(Clone, Debug)]
pub struct XdpImage<'a> {
    pub name: &'a str,
    pub path: &'a std::path::Path,
}

/// Chip selector (re-export style string for open; mapped to espflash).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Esp32c3,
}

impl Chip {
    pub(crate) fn as_espflash(self) -> espflash::target::Chip {
        match self {
            Self::Esp32c3 => espflash::target::Chip::Esp32c3,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Esp32c3 => "esp32c3",
        }
    }
}

impl fmt::Display for Chip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Helper to format open port for SessionWhy without owning PathBuf long-term.
pub(crate) fn port_label(port: Option<&std::path::Path>) -> String {
    port.map(|p| p.display().to_string())
        .unwrap_or_else(|| "(auto)".into())
}

#[allow(dead_code)]
pub(crate) fn path_buf_label(p: &Path) -> String {
    p.display().to_string()
}
