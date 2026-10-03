//! App-mode serial: sync atomic request (open → cmd → reply → close).

mod log;
mod reply;

pub use log::Log;
pub use reply::perf::{self, Report, decode as decode_perf, mask_for_len};
pub use reply::ReplyMsg;

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::client::error::{Error, Result};
use crate::client::port::Port;
use crate::client::{DEFAULT_REQUEST_TIMEOUT, GET_MAP_TIMEOUT};
use crate::r#type::ops::Command;
use crate::r#type::reply::is_reply_prefix;

use self::reply::collect_reply;

/// Handle for issuing App commands (sync, open-per-request).
pub struct SerialHandle {
    port: Arc<Port>,
    log: Arc<Log>,
}

impl SerialHandle {
    pub fn new(port: Arc<Port>, log: Arc<Log>) -> Self {
        Self { port, log }
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
        let result = self.port.with_app(|mut app| {
            app.write_line(&line)?;
            let app = RefCell::new(app);
            let mut read_line = |t| app.borrow_mut().read_line(t);
            let mut read_exact = |n, t| app.borrow_mut().read_exact(n, t);
            // Port is exclusive for this op; flash cannot steal mid-request.
            let mut interrupt = || false;
            collect_reply(
                &mut read_line,
                &mut read_exact,
                cmd.is_get_map(),
                cmd.is_get_perf(),
                req_timeout,
                &mut interrupt,
            )
        });
        match &result {
            Ok(msg) => log.write_line(&format!("[cmd] ← {}", msg.reply)),
            Err(e) => log.write_line(&format!("[err] request: {e}")),
        }
        result
    }

    /// Best-effort drain of pending UART into the log.
    pub fn drain(&self) -> Result<()> {
        let log = Arc::clone(&self.log);
        self.port.with_app(|mut app| {
            loop {
                match app.read_line(Duration::from_millis(1)) {
                    Ok(Some(line)) => {
                        if is_reply_prefix(&line) {
                            log.write_line(&format!("[cmd] ← {line}"));
                        } else {
                            log.write_line(&format!("[uart] {line}"));
                        }
                    }
                    _ => break,
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

/// Create log + bind to an already-resolved [`Port`].
pub fn open(port: Arc<Port>, log_path: &Path, tee_stdout: bool) -> Result<SerialHandle> {
    let log = Arc::new(Log::create(log_path, tee_stdout)?);
    Ok(SerialHandle::new(port, log))
}
