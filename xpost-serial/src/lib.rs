//! Host serial / flash backend for `xpost-agent`.
//!
//! Library only: no binary. Exclusive USB-UART with a seq-ordered event
//! transcript ([`Sink`]) so runs can be browsed like a file.

mod error;
mod event;
mod partitions;
mod session;
mod sink;
mod table;

pub use error::{Error, Result};
pub use event::{
    Chip, Dir, Event, FlashPhase, Image, IoData, Kind, Mode, SessionWhy, XdpImage,
};
pub use session::Session;
pub use sink::{Sink, StdoutSink, VecSink};
pub use table::{
    ColdTable, InstallOutcome, JUST_DROP_TABLE, XdpEntry, list_entries, plan_install,
    xdp_name_from_bytes, xdp_name_from_file,
};
