//! App-mode serial: `serialport` + transcript [`Log`], sync request via UART select.
//!
//! Replaces the old `port::app` session. External API stays ordinary (`request` /
//! `drain`); waiting is block-select: business / interrupt / timeout.

mod log;
mod reply;
mod uart;

pub use log::Log;
pub use reply::perf::{self, Report, decode as decode_perf, mask_for_len};
pub use reply::ReplyMsg;
pub use uart::{Event, Uart};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::client::error::{Error, Result};
use crate::client::port::Port;
use crate::client::{BAUD, DEFAULT_REQUEST_TIMEOUT, GET_MAP_TIMEOUT};
use crate::r#type::ops::Command;
use crate::r#type::reply::is_reply_prefix;

use self::reply::collect_reply;

/// Handle for issuing App commands (sync, open-per-request).
pub struct SerialHandle {
    port: Arc<Port>,
    log: Arc<Log>,
    /// Flash worker signals here before taking the port gate.
    interrupt_rx: Receiver<()>,
}

impl SerialHandle {
    pub fn new(port: Arc<Port>, log: Arc<Log>, interrupt_rx: Receiver<()>) -> Self {
        Self {
            port,
            log,
            interrupt_rx,
        }
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn log_arc(&self) -> Arc<Log> {
        Arc::clone(&self.log)
    }

    pub fn port(&self) -> &Port {
        &self.port
    }

    /// Send an App command and wait for the matching reply (blocking, atomic).
    pub fn request(&self, cmd: Command) -> Result<ReplyMsg> {
        if cmd.is_flash() {
            return Err(Error::Msg(
                "Flash commands belong on the flash path".into(),
            ));
        }
        let req_timeout = if cmd.is_get_map() {
            GET_MAP_TIMEOUT
        } else {
            DEFAULT_REQUEST_TIMEOUT
        };
        let line = cmd.to_string();
        self.log.write_line(&format!("[cmd] → {line}"));
        let log = Arc::clone(&self.log);
        let interrupt = self.interrupt_rx.clone();
        let name = self.port.name().to_string();
        let result = self.port.with_gate(|| {
            let mut uart = Uart::open(&name, BAUD, interrupt)?;
            uart.write_line(&line)?;
            let mut wait = |t| {
                let ev = uart.select(t)?;
                if let Event::Line(ref l) = ev {
                    if is_reply_prefix(l) {
                        log.write_line(&format!("[cmd] ← {l}"));
                    } else {
                        log.write_line(&format!("[uart] {l}"));
                    }
                }
                Ok(ev)
            };
            collect_reply(
                &mut wait,
                cmd.is_get_map(),
                cmd.is_get_perf(),
                req_timeout,
            )
        });
        match &result {
            Ok(msg) => {
                // Header already logged in wait; note final OK shape for get map / perf.
                if cmd.is_get_map() || cmd.is_get_perf() {
                    log.write_line(&format!("[cmd] ← {}", msg.reply));
                }
            }
            Err(e) => log.write_line(&format!("[err] request: {e}")),
        }
        result
    }

    /// Best-effort drain of pending UART into the log.
    pub fn drain(&self) -> Result<()> {
        let log = Arc::clone(&self.log);
        let interrupt = self.interrupt_rx.clone();
        let name = self.port.name().to_string();
        self.port.with_gate(|| {
            let uart = Uart::open(&name, BAUD, interrupt)?;
            loop {
                match uart.select(Duration::from_millis(1)) {
                    Ok(Event::Line(line)) => {
                        if is_reply_prefix(&line) {
                            log.write_line(&format!("[cmd] ← {line}"));
                        } else {
                            log.write_line(&format!("[uart] {line}"));
                        }
                    }
                    Ok(Event::Blob(_)) => {}
                    Err(Error::RequestTimeout(_)) => break,
                    Err(Error::Interrupted) => break,
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })
    }

    pub fn lines_in_range(
        &self,
        since: std::time::SystemTime,
        until: std::time::SystemTime,
    ) -> Result<Vec<String>> {
        Ok(self.log.lines_in_range(since, until)?)
    }
}

/// Create log + interrupt channel end; bind to an already-resolved [`Port`].
pub fn open(
    port: Arc<Port>,
    log_path: &Path,
    tee_stdout: bool,
    interrupt_rx: Receiver<()>,
) -> Result<SerialHandle> {
    let log = Arc::new(Log::create(log_path, tee_stdout)?);
    Ok(SerialHandle::new(port, log, interrupt_rx))
}

/// Flash side of the interrupt pair (try-send before taking the port gate).
pub type InterruptTx = Sender<()>;

pub fn interrupt_channel() -> (InterruptTx, Receiver<()>) {
    crossbeam_channel::bounded(1)
}
