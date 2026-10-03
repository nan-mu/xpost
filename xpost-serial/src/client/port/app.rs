//! App-mode UART session (open for one atomic request / drain).
//!
//! Sync, single-threaded: no background reader. Drop closes the port.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

use serialport::SerialPort;

use crate::client::error::{Error, Result};
use crate::client::PORT_IO_TIMEOUT;

/// Short-lived App UART handle. Drop closes the port.
pub struct AppSession {
    port: BufReader<Box<dyn SerialPort>>,
}

impl AppSession {
    pub(crate) fn open(name: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(name, baud)
            .timeout(PORT_IO_TIMEOUT)
            .open()?;
        Ok(Self {
            port: BufReader::new(port),
        })
    }

    pub fn write_line(&mut self, line: &str) -> Result<()> {
        let w = self.port.get_mut();
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()?;
        Ok(())
    }

    /// One line, or `Ok(None)` on I/O timeout (no data within `timeout`).
    ///
    /// `Ok(0)` / EOF is an error — serial ports do not "idle" as zero-length reads
    /// when a read timeout is configured (`TimedOut` is the idle path).
    pub fn read_line(&mut self, timeout: Duration) -> Result<Option<String>> {
        self.port.get_mut().set_timeout(timeout)?;
        let mut buf = String::new();
        match self.port.read_line(&mut buf) {
            Ok(0) => Err(Error::Msg("serial EOF".into())),
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']).to_string();
                if line.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(line))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Read exactly `n` bytes (uses `BufReader` so bytes after a perf header stay framed).
    pub fn read_exact(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut payload = vec![0u8; n];
        let mut filled = 0;
        while filled < n {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::RequestTimeout(timeout));
            }
            self.port
                .get_mut()
                .set_timeout(remaining.min(PORT_IO_TIMEOUT))?;
            match self.port.read(&mut payload[filled..]) {
                Ok(0) => return Err(Error::Msg("serial EOF".into())),
                Ok(k) => filled += k,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(payload)
    }
}
