//! Typed serial transcript events. Ordered by [`Event::seq`] like a file.

use std::fmt;
use std::path::PathBuf;
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
            Self::ResetToApp => "reset_to_app".into(),
            Self::ResetToDownload => "reset_to_download".into(),
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

/// Host perspective: Tx = bytes written toward the MCU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Tx,
    Rx,
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
        phase: FlashPhase,
        addr: Option<u32>,
        len: u32,
        done: u32,
    },
    Io {
        dir: Dir,
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
                dir: Dir::Tx,
                data: IoData::Line(s),
            } => format!("> {s}"),
            Kind::Io {
                dir: Dir::Rx,
                data: IoData::Line(s),
            } => s.clone(),
            Kind::Session { why, .. } => format!("--- {} ---", why.label()),
            Kind::Flash {
                phase,
                addr,
                len,
                done,
            } => {
                let addr = addr
                    .map(|a| format!("{a:#x}"))
                    .unwrap_or_else(|| "-".into());
                format!("--- flash {phase} addr={addr} {done}/{len} ---")
            }
            Kind::Error { ctx, msg } => format!("--- error: {ctx}: {msg} ---"),
        }
    }
}

/// Firmware + optional cold-region Compact ELF for [`crate::Session::program`].
#[derive(Clone, Debug)]
pub struct Image<'a> {
    pub firmware: &'a std::path::Path,
    pub partitions: &'a std::path::Path,
    pub xdp: Option<XdpImage<'a>>,
}

/// Raw Compact ELF write-bin target.
#[derive(Clone, Debug)]
pub struct XdpImage<'a> {
    pub addr: u32,
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
pub(crate) fn path_buf_label(p: &PathBuf) -> String {
    p.display().to_string()
}
