//! Host-only control-plane shapes (flash / chip). Wire App commands live in
//! [`crate::r#type::command::Command`].

use std::fmt;
use std::sync::Arc;

/// Lifecycle markers written to the transcript around a flash op.
pub enum FlashEvent {
    Begin(FlashOp),
    End(FlashOp),
    Failed { op: FlashOp, msg: String },
}

#[derive(Clone)]
pub struct FirmwareExpectation {
    pub elf: Vec<u8>,
    pub sha256: [u8; 32],
    pub partition_table: Vec<u8>,
}

/// Flash worker inbox / lifecycle payload.
#[derive(Clone)]
pub enum FlashOp {
    Fw {
        snapshot: FirmwareExpectation,
    },
    Xdp {
        name: String,
        bytes: Arc<[u8]>,
        expectation: FirmwareExpectation,
        replace_single: bool,
    },
    Clear {
        /// Empty = clear all (`*`).
        names: Vec<String>,
    },
    /// Read the installed firmware identity and one cold image without writing flash.
    Attest {
        expectation: FirmwareExpectation,
    },
    /// Read the XDP table through an exclusive download-mode session.
    List,
}

/// Target chip for download-mode flash.
pub enum Chip {
    Esp32c3,
}

impl FlashOp {
    pub fn label(&self) -> String {
        match self {
            Self::Fw { .. } => "fw".into(),
            Self::Xdp { name, .. } => format!("xdp name={name}"),
            Self::Clear { names } if names.is_empty() => "clear xdp *".into(),
            Self::Clear { names } => format!("clear xdp {}", names.join(",")),
            Self::List => "get xdp".into(),
            Self::Attest { .. } => "attest deployment".into(),
        }
    }
}

impl fmt::Display for FlashEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Begin(op) => write!(f, "FLASH begin {}", op.label()),
            Self::End(op) => write!(f, "FLASH end {}", op.label()),
            Self::Failed { op, msg } => write!(f, "FLASH failed {} {msg}", op.label()),
        }
    }
}

impl fmt::Display for FlashOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

impl fmt::Display for Chip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Esp32c3 => write!(f, "esp32c3"),
        }
    }
}

impl fmt::Debug for Chip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Copy for Chip {}
impl Clone for Chip {
    fn clone(&self) -> Self {
        *self
    }
}
