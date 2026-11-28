//! App UART: write path + background reader that appends into [`SharedRecord`].

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::SystemTime;

use tokio::sync::Notify;

use super::byte_port::{AppBytePort, open_usb};
use crate::client::APP_READ_POLL;
use crate::client::PORT_IO_TIMEOUT;
use crate::client::record::SharedRecord;

/// Bytes that can be written as one App UART command (no trailing newline).
pub trait Command {
    fn to_serial_buf(&self) -> &[u8];
}

impl Command for [u8] {
    fn to_serial_buf(&self) -> &[u8] {
        self
    }
}

impl Command for Vec<u8> {
    fn to_serial_buf(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Command for str {
    fn to_serial_buf(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl Command for String {
    fn to_serial_buf(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl<T: Command + ?Sized> Command for &T {
    fn to_serial_buf(&self) -> &[u8] {
        (**self).to_serial_buf()
    }
}

/// Flash → App cancel signal (interrupt arm of request `select!`).
#[derive(Clone, Default)]
pub struct Interrupt {
    inner: Arc<InterruptInner>,
}

#[derive(Default)]
struct InterruptInner {
    pending: AtomicBool,
    notify: Notify,
}

impl std::fmt::Debug for Interrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interrupt")
            .field("pending", &self.inner.pending.load(Ordering::Relaxed))
            .finish()
    }
}

impl Interrupt {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn signal(&self) {
        self.inner.pending.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    pub fn clear(&self) {
        self.inner.pending.store(false, Ordering::Release);
    }

    pub async fn wait(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.inner.pending.swap(false, Ordering::AcqRel) {
                return;
            }
            notified.await;
            if self.inner.pending.swap(false, Ordering::AcqRel) {
                return;
            }
        }
    }
}

/// Short-lived App UART. Reader thread continuously writes into [`SharedRecord`].
pub struct Uart {
    writer: Option<Box<dyn AppBytePort>>,
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
    /// Open App UART over a host USB serial device.
    ///
    /// Spawns a reader that appends every framed line/blob into `record` until drop.
    pub(crate) fn open_usb(name: &str, baud: u32, record: SharedRecord) -> io::Result<Self> {
        Self::open_port(open_usb(name, baud)?, record)
    }

    /// Open App UART over any cloneable [`AppBytePort`] (USB or memory).
    pub(crate) fn open_port(
        mut port: Box<dyn AppBytePort>,
        record: SharedRecord,
    ) -> io::Result<Self> {
        // Reader polls briefly: `Uart::drop` joins it, so a long read timeout
        // would add that wait to every command round trip.
        port.set_timeout(APP_READ_POLL)?;
        let mut writer = port.try_clone_box()?;
        writer.set_timeout(PORT_IO_TIMEOUT)?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let join = thread::spawn(move || reader_loop(port, record, stop_t));
        Ok(Self {
            writer: Some(writer),
            stop,
            join: Some(join),
        })
    }

    /// Write `cmd` and its newline, then flush the port.
    ///
    /// Return the time before the newline as the lower bound for matching replies.
    pub fn write_command<C: Command + ?Sized>(&mut self, cmd: &C) -> io::Result<SystemTime> {
        let Some(w) = self.writer.as_mut() else {
            return Err(std::io::Error::other("uart closed"));
        };
        w.write_all(cmd.to_serial_buf())?;
        // The DUT can reply as soon as it receives the newline, while flush is pending.
        let sent = SystemTime::now();
        w.write_all(b"\n")?;
        w.flush()?;
        Ok(sent)
    }
}

fn reader_loop(port: Box<dyn AppBytePort>, record: SharedRecord, stop: Arc<AtomicBool>) {
    let mut reader = BufReader::new(port);
    let mut buf = String::new();
    while !stop.load(Ordering::Acquire) {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let line = buf.trim_end_matches(['\r', '\n', '\0']).to_string();
                if line.is_empty() {
                    continue;
                }
                let line_b = line.as_bytes();
                let blob_len = crate::client::reply::parse_get_perf_len(line_b)
                    .or_else(|| crate::client::reply::parse_get_map_len(line_b))
                    .or_else(|| crate::client::reply::parse_perf_read_len(line_b))
                    .or_else(|| crate::client::reply::parse_exec_batch_read_len(line_b));
                if let Some(n) = blob_len {
                    record.uart(line_b);
                    match read_exact_wake(&mut reader, n, &stop) {
                        Ok(payload) => record.blob(payload),
                        Err(_) => break,
                    }
                } else {
                    record.uart(line_b);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(_) => break,
        }
    }
}

fn read_exact_wake(
    reader: &mut BufReader<Box<dyn AppBytePort>>,
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

#[cfg(all(test, feature = "dev", feature = "server"))]
mod tests {
    use super::*;
    use crate::client::record::{Found, SharedRecordInner};
    use crate::tests::harness::TestError;
    use std::time::Duration;

    struct ReplyDuringFlush(SharedRecord);

    impl Read for ReplyDuringFlush {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for ReplyDuringFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes != b"\n" {
                // This reply arrived before the new command was terminated.
                self.0.uart(b"OK exec batch invoke 1 10 11 0");
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            // Replay the valid DUT reply while the host is still flushing the write.
            self.0
                .uart(b"OK exec batch invoke 903449073 88323818 88323822 0");
            Ok(())
        }
    }

    impl AppBytePort for ReplyDuringFlush {
        fn try_clone_box(&self) -> io::Result<Box<dyn AppBytePort>> {
            Ok(Box::new(Self(self.0.clone())))
        }

        fn set_timeout(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn invoke_accepts_a_reply_received_before_flush_returns() {
        let record = SharedRecordInner::create_memory();
        let mut uart =
            Uart::open_port(Box::new(ReplyDuringFlush(record.clone())), record.clone()).unwrap();
        let command = crate::command::Command::ExecBatchInvoke;
        let sent = uart.write_command(command.to_string().as_str()).unwrap();
        let (found, _) = tokio::time::timeout(
            Duration::from_millis(30),
            record.wait_reply::<TestError>(&command, sent),
        )
        .await
        .expect("the recorded reply must not expire")
        .unwrap();
        assert!(matches!(
            found,
            Found::ExecBatchInvoke {
                boot_id: 903449073,
                begin_tick: 88323818,
                end_tick: 88323822,
                action: 0,
            }
        ));
    }
}
