//! App UART: `serialport` + framing reader; business waits via block [`select`](crossbeam_channel::select).
//!
//! Serial fd timeout is only a short wake for the reader (`PORT_IO_TIMEOUT`) so it can
//! observe `stop`. Request deadline and flash interrupt are **not** on the fd — they are
//! select arms next to the business channel.

use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serialport::SerialPort;

use crate::client::error::{Error, Result};
use crate::client::PORT_IO_TIMEOUT;

/// Framed UART event for the business arm of [`Uart::select`].
#[derive(Debug)]
pub enum Event {
    Line(String),
    Blob(Vec<u8>),
}

/// Short-lived App UART. Drop signals the reader and closes the port.
pub struct Uart {
    writer: Option<Box<dyn SerialPort>>,
    rx: Receiver<Event>,
    interrupt: Receiver<()>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for Uart {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.writer.take();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Uart {
    /// Open port under the caller's [`crate::client::port::Port`] gate.
    ///
    /// `interrupt` is cloned from the client-wide flash-cancel channel (drained on open).
    pub fn open(name: &str, baud: u32, interrupt: Receiver<()>) -> Result<Self> {
        while interrupt.try_recv().is_ok() {}

        let port = serialport::new(name, baud)
            .timeout(PORT_IO_TIMEOUT)
            .open()?;
        let writer = port.try_clone()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let (tx, rx) = crossbeam_channel::unbounded();
        let join = thread::spawn(move || reader_loop(port, tx, stop_t));
        Ok(Self {
            writer: Some(writer),
            rx,
            interrupt,
            stop,
            join: Some(join),
        })
    }

    pub fn write_line(&mut self, line: &str) -> Result<()> {
        let w = self
            .writer
            .as_mut()
            .ok_or_else(|| Error::Msg("uart closed".into()))?;
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()?;
        Ok(())
    }

    /// Block until **business** / **interrupt** / **timeout**.
    pub fn select(&self, timeout: Duration) -> Result<Event> {
        crossbeam_channel::select! {
            recv(self.rx) -> msg => match msg {
                Ok(ev) => Ok(ev),
                Err(_) => Err(Error::Msg("uart reader died".into())),
            },
            recv(self.interrupt) -> _ => Err(Error::Interrupted),
            default(timeout) => Err(Error::RequestTimeout(timeout)),
        }
    }
}

fn reader_loop(port: Box<dyn SerialPort>, tx: Sender<Event>, stop: Arc<AtomicBool>) {
    let mut reader = BufReader::new(port);
    let mut buf = String::new();
    while !stop.load(Ordering::Acquire) {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n']).to_string();
                if line.is_empty() {
                    continue;
                }
                if let Some(n) = crate::r#type::reply::parse_ok_get_perf_len(&line) {
                    if tx.send(Event::Line(line)).is_err() {
                        break;
                    }
                    match read_exact_wake(&mut reader, n, &stop) {
                        Ok(payload) => {
                            if tx.send(Event::Blob(payload)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                } else if tx.send(Event::Line(line)).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
    }
}

/// Fill `n` bytes using only the port's short wake timeout; observe `stop` between reads.
fn read_exact_wake(
    reader: &mut BufReader<Box<dyn SerialPort>>,
    n: usize,
    stop: &AtomicBool,
) -> std::io::Result<Vec<u8>> {
    let mut payload = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        if stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "uart stop",
            ));
        }
        match reader.read(&mut payload[filled..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "serial EOF",
                ));
            }
            Ok(k) => filled += k,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(payload)
}
