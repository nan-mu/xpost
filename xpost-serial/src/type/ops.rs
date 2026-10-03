//! Host control-plane ops (shared shapes for client serial/flash channels).
//!
//! Wire UART lines live in [`super::command`]; this module is the host-side
//! command tree (incl. flash lifecycle). Path fields are `String` for `no_std`.

use alloc::string::String;
use alloc::vec::Vec;

/// Unified host command. App variants map to wire lines; [`Command::Flash`] is
/// flash→serial interrupt / log only (never written to DUT UART).
#[derive(Clone, PartialEq, Eq)]
pub enum Command {
    Ping,
    Enable(Enable),
    Disable(Disable),
    Clear(Clear),
    Get(Get),
    Update(Update),
    Flash(Flash),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Enable {
    Xdp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Disable {
    Xdp,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Clear {
    Map { name: String },
    Perf,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Get {
    Map { name: String },
    Perf,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Update {
    pub name: String,
    pub key: String,
    pub value: String,
}

#[derive(Clone, PartialEq, Eq)]
pub enum Flash {
    Begin(FlashOp),
    End(FlashOp),
    Failed { op: FlashOp, msg: String },
}

/// Flash worker inbox / lifecycle payload.
#[derive(Clone, PartialEq, Eq)]
pub enum FlashOp {
    Fw {
        firmware: String,
        partitions: String,
    },
    Xdp {
        name: String,
        path: String,
    },
    Clear {
        /// Empty = clear all (`*`).
        names: Vec<String>,
    },
    /// Read cold table (download); still interrupts App UART.
    List,
}

/// Target chip for download-mode flash.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Esp32c3,
}
