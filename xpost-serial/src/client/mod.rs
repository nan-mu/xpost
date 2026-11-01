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
/// Brief pause after hard-reset before releasing the download fd.
pub(crate) const RESET_TO_APP_SETTLE: Duration = Duration::from_millis(50);
/// Keep App UART open after flash so a fast unsolicited `OK init` is recorded
/// (XDP firmware emits it before Wi-Fi; HAL waits for DHCP+bind).
pub(crate) const POST_FLASH_INIT_CAPTURE: Duration = Duration::from_secs(3);
/// Reader-thread wake only — never the business request deadline.
pub(crate) const PORT_IO_TIMEOUT: Duration = Duration::from_millis(200);

/// Host open options. Transcript goes under [`Self::log_dir`] as rotating
/// `serial.log` / `serial.log.N` (see [`crate::client::record`]).
pub struct OpenOptions<'a> {
    pub port: Option<&'a Path>,
    pub chip: Chip,
    /// Directory for rotating serial transcript files.
    pub log_dir: &'a Path,
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
        let record = SharedRecordInner::create_rotating(opts.log_dir, opts.tee_stdout)?;
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

    /// Like [`Self::open_memory`], but tees the session transcript to a single file
    /// (no rotation — test harness only).
    #[cfg(all(test, feature = "dev"))]
    pub fn open_memory_with_log(client: MemoryClient, log_path: &Path) -> io::Result<Self> {
        Ok(Self {
            link: AppLink::Memory(client),
            record: SharedRecordInner::create_file(log_path, false)?,
            flash: None,
        })
    }

    /// Read the entire transcript file (empty if this handler has no log path).
    #[cfg(all(test, feature = "dev"))]
    pub fn log_lines(&self) -> io::Result<Vec<String>> {
        self.record.lock().all_lines()
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
        self.request_at(cmd).map(|(reply, _)| reply)
    }

    /// Like [`Self::request`], plus the time the reader thread read the reply
    /// line. That is the instant the host received the MCU reply, not the later
    /// moment the caller formats it.
    pub fn request_at<'a, E>(&'a self, cmd: &Command<'_>) -> core::result::Result<(OkReply<'a>, SystemTime), E>
    where
        E: RequestError,
    {
        let (found, at) = block_fut(serial::run_op::<E>(&self.link, &self.record, cmd))?;
        Ok((self.record.materialize_ok::<E>(&found)?, at))
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
