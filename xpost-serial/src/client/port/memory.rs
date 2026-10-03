//! In-memory App UART duplex for `dev` injectable-IO tests.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::byte_port::AppBytePort;
use super::uart::Interrupt;
use crate::client::PORT_IO_TIMEOUT;

struct Queue {
    buf: Mutex<VecDeque<u8>>,
    cv: Condvar,
}

impl Queue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            buf: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
        })
    }

    fn push_all(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        g.extend(bytes.iter().copied());
        self.cv.notify_all();
    }

    fn pop_one_nonblocking(&self) -> Option<u8> {
        let mut g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        g.pop_front()
    }

    fn read_timeout(&self, dst: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }
        let deadline = Instant::now() + timeout;
        let mut g = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if !g.is_empty() {
                let n = dst.len().min(g.len());
                for slot in dst.iter_mut().take(n) {
                    *slot = g.pop_front().unwrap();
                }
                return Ok(n);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "memory uart idle"));
            }
            let (guard, wait) = self
                .cv
                .wait_timeout(g, deadline.saturating_duration_since(now))
                .unwrap_or_else(|e| e.into_inner());
            g = guard;
            if wait.timed_out() && g.is_empty() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "memory uart idle"));
            }
        }
    }
}

/// One end of a memory duplex (`Read`/`Write` + clone for UART reader/writer).
#[derive(Clone)]
pub struct MemoryIo {
    rx: Arc<Queue>,
    tx: Arc<Queue>,
    timeout: Duration,
}

impl MemoryIo {
    fn new(rx: Arc<Queue>, tx: Arc<Queue>) -> Self {
        Self {
            rx,
            tx,
            timeout: PORT_IO_TIMEOUT,
        }
    }
}

impl Read for MemoryIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.rx.read_timeout(buf, self.timeout)
    }
}

impl Write for MemoryIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tx.push_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AppBytePort for MemoryIo {
    fn try_clone_box(&self) -> io::Result<Box<dyn AppBytePort>> {
        Ok(Box::new(self.clone()))
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.timeout = timeout;
        Ok(())
    }
}

/// Client-facing memory link (host side of the duplex).
#[derive(Clone)]
pub struct MemoryClient {
    io: MemoryIo,
    interrupt: Interrupt,
}

impl MemoryClient {
    pub(crate) fn io(&self) -> MemoryIo {
        self.io.clone()
    }

    pub(crate) fn interrupt(&self) -> &Interrupt {
        &self.interrupt
    }
}

/// Server-facing memory link (DUT side of the duplex).
#[derive(Clone)]
pub struct MemoryServer {
    io: MemoryIo,
}

impl MemoryServer {
    /// Non-blocking RX for [`crate::server::SerialHandler`] read adapter.
    pub fn read_byte(&self) -> Option<u8> {
        self.io.rx.pop_one_nonblocking()
    }

    /// Infallible TX for server write adapter.
    pub fn write_all(&self, bytes: &[u8]) -> Result<(), Infallible> {
        self.io.tx.push_all(bytes);
        Ok(())
    }
}

/// Paired client/server ends of one App-mode memory UART.
pub struct MemoryPair {
    pub client: MemoryClient,
    pub server: MemoryServer,
}

impl MemoryPair {
    pub fn new() -> Self {
        let host_to_dut = Queue::new();
        let dut_to_host = Queue::new();
        Self {
            client: MemoryClient {
                io: MemoryIo::new(Arc::clone(&dut_to_host), Arc::clone(&host_to_dut)),
                interrupt: Interrupt::new(),
            },
            server: MemoryServer {
                io: MemoryIo::new(host_to_dut, dut_to_host),
            },
        }
    }
}

impl Default for MemoryPair {
    fn default() -> Self {
        Self::new()
    }
}
