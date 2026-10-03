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

pub use crate::r#type::command::Command;
pub use crate::r#type::reply::OkReply;
pub use error::{Interrupted, ParseError, RequestError, RequestTimeout, Result};
pub use flash::{
    DEFAULT_XDP_PARTITION, FlashConfig, FlashResult, InstallOutcome, XdpEntry, XdpTable,
};
pub use ops::{Chip, FlashOp};
pub use port::Port;
#[cfg(all(test, feature = "dev"))]
pub use port::{MemoryClient, MemoryPair, MemoryServer};
pub use serial::{Report, decode_perf, mask_for_len};

use flash::FlashWorker;
use port::AppLink;
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

/// Unified host control-plane handle (App UART + optional flash worker).
pub struct SerialHandler {
    link: AppLink,
    record: SharedRecord,
    flash: Option<FlashWorker>,
}

impl SerialHandler {
    /// Resolve USB port, bind App + flash worker (no long-lived fd on the port).
    pub fn open(opts: OpenOptions<'_>) -> io::Result<Self> {
        let port = Arc::new(Port::resolve(opts.port, opts.chip)?);
        let record = SharedRecordInner::create(opts.log_path, opts.tee_stdout)?;
        let flash = flash::spawn(opts.flash, Arc::clone(&port), Arc::clone(&record));
        Ok(Self {
            link: AppLink::Usb(port),
            record,
            flash: Some(flash),
        })
    }

    /// App-only memory link for injectable-IO tests (`dev` + `cfg(test)`).
    /// No flash worker / log file.
    #[cfg(all(test, feature = "dev"))]
    pub fn open_memory(client: MemoryClient) -> Self {
        Self {
            link: AppLink::Memory(client),
            record: SharedRecordInner::create_memory(),
            flash: None,
        }
    }

    /// Stop the flash worker (if any) and release this handler.
    pub fn shutdown(self) {
        if let Some(flash) = self.flash {
            flash.shutdown();
        }
    }

    /// Send one App wire [`Command`] and wait for the matching reply.
    pub fn request<'a, E>(&'a self, cmd: &Command<'_>) -> Result<'a, E>
    where
        E: RequestError,
    {
        let found = block_fut(serial::run_op::<E>(&self.link, &self.record, cmd))?;
        self.record.materialize_ok::<E>(&found)
    }

    /// Run a download-mode [`FlashOp`] (not an App-mode wire reply).
    pub fn flash(&self, op: FlashOp) -> io::Result<FlashResult> {
        self.flash
            .as_ref()
            .ok_or_else(|| io::Error::other("flash worker not available on this link"))?
            .call(op)
    }

    pub fn lines_in_range(&self, since: SystemTime, until: SystemTime) -> io::Result<Vec<String>> {
        self.record.lock().lines_in_range(since, until)
    }

    pub fn port(&self) -> &Port {
        self.link
            .port()
            .expect("SerialHandler::port requires a USB App link")
    }
}

fn block_fut<F: core::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.block_on(fut),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime for App request")
            .block_on(fut),
    }
}
