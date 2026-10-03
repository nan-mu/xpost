//! Host USB control plane: sync App UART + flash task sharing [`port::Port`].

pub mod error;
pub mod flash;
pub mod ops;
pub mod port;
pub mod record;
pub mod reply;
pub mod serial;
mod wire_fmt;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures::executor::block_on;

pub use crate::r#type::command::Command;
pub use crate::r#type::reply::OkReply;
pub use error::{Interrupted, ParseError, RequestError, RequestTimeout, Result};
pub use flash::{
    DEFAULT_XDP_PARTITION, FlashConfig, FlashResult, InstallOutcome, XdpEntry, XdpTable,
};
pub use ops::{Chip, FlashOp};
pub use port::Port;
pub use serial::{Report, decode_perf, mask_for_len};

use flash::FlashWorker;
use record::{SharedRecord, SharedRecordInner};

// —— shared client constants (single source) ——

pub(crate) const BAUD: u32 = 115_200;
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const GET_MAP_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const DEFAULT_IMAGE_CAPACITY: usize = 0x1000;
pub(crate) const FLASH_SECTOR: u32 = 0x1000;
pub(crate) const READ_BLOCK: u32 = 0x1000;
pub(crate) const READ_MAX_IN_FLIGHT: u32 = 64;
pub(crate) const RESET_TO_APP_SETTLE: Duration = Duration::from_millis(800);
/// Reader-thread wake only — never the business request deadline.
pub(crate) const PORT_IO_TIMEOUT: Duration = Duration::from_millis(200);

pub struct OpenOptions<'a> {
    pub port: Option<&'a Path>,
    pub chip: Chip,
    pub log_path: &'a Path,
    pub tee_stdout: bool,
    pub flash: FlashConfig,
}

/// Unified host control-plane handle (App UART + flash worker).
pub struct SerialHandler {
    port: Arc<Port>,
    record: SharedRecord,
    flash: FlashWorker,
}

impl SerialHandler {
    /// Resolve USB port, bind App + flash worker (no long-lived fd on the port).
    pub fn open(opts: OpenOptions<'_>) -> io::Result<Self> {
        let port = Arc::new(Port::resolve(opts.port, opts.chip)?);
        let record = SharedRecordInner::create(opts.log_path, opts.tee_stdout)?;
        let flash = flash::spawn(opts.flash, Arc::clone(&port), Arc::clone(&record));
        Ok(Self {
            port,
            record,
            flash,
        })
    }

    /// Stop the flash worker and release this handler.
    pub fn shutdown(self) {
        self.flash.shutdown();
    }

    /// Send one App wire [`Command`] and wait for the matching reply.
    pub fn request<'a, E>(&'a self, cmd: &Command<'_>) -> Result<'a, E>
    where
        E: RequestError,
    {
        let found = block_on(serial::run_op::<E>(&self.port, &self.record, cmd))?;
        self.record.materialize_ok::<E>(&found)
    }

    /// Run a download-mode [`FlashOp`] (not an App-mode wire reply).
    pub fn flash(&self, op: FlashOp) -> io::Result<FlashResult> {
        self.flash.call(op)
    }

    pub fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> io::Result<Vec<String>> {
        self.record.lock().lines_in_range(since, until)
    }

    pub fn port(&self) -> &Port {
        &self.port
    }
}
