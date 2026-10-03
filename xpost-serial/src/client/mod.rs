//! Host USB control plane: sync App UART + flash task sharing [`port::Port`].

pub mod command;
pub mod error;
pub mod flash;
pub mod port;
pub mod serial;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub use crate::r#type::ops::{
    Chip, Clear, Command, Disable, Enable, Flash, FlashOp, Get, Update,
};
pub use error::{Error, Result};
pub use flash::{
    DEFAULT_XDP_PARTITION, FlashConfig, FlashHandle, FlashResult, InstallOutcome, XdpEntry,
    XdpTable, xdp_name_from_bytes, xdp_name_from_file, xdp_table_from_csv,
};
pub use port::Port;
pub use serial::{Log, ReplyMsg, Report, SerialHandle, decode_perf, mask_for_len};

// —— shared client constants (single source) ——

pub(crate) const BAUD: u32 = 115_200;
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const GET_MAP_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_IMAGE_CAPACITY: usize = 0x1000;
pub(crate) const FLASH_SECTOR: u32 = 0x1000;
pub(crate) const READ_BLOCK: u32 = 0x1000;
pub(crate) const READ_MAX_IN_FLIGHT: u32 = 64;
pub(crate) const RESET_TO_APP_SETTLE: Duration = Duration::from_millis(800);
pub(crate) const PORT_IO_TIMEOUT: Duration = Duration::from_millis(200);

/// Paired serial + flash handles sharing one [`Port`] (open-per-op).
pub struct Client {
    pub port: Arc<Port>,
    pub serial: SerialHandle,
    pub flash: FlashHandle,
}

pub struct OpenOptions<'a> {
    pub port: Option<&'a Path>,
    pub chip: Chip,
    pub log_path: &'a Path,
    pub tee_stdout: bool,
    pub flash: FlashConfig,
}

impl Client {
    /// Resolve USB port, bind App + flash handles (no long-lived fd on the port).
    ///
    /// App requests are sync atomic (`Port::with_app`). Flash opens download mode
    /// per op (`Port::with_download`). Matches idf-monitor pause-before-flash.
    pub fn open(opts: OpenOptions<'_>) -> Result<Self> {
        let port = Arc::new(Port::resolve(opts.port, opts.chip)?);
        let serial = serial::open(Arc::clone(&port), opts.log_path, opts.tee_stdout)?;
        let flash = flash::spawn(opts.flash, Arc::clone(&port), serial.log_arc());
        Ok(Self {
            port,
            serial,
            flash,
        })
    }
}
