//! Cloneable App-mode byte duplex used by [`super::uart::Uart`].

use std::io::{self, Read, Write};
use std::time::Duration;

use serialport::SerialPort;

use crate::client::PORT_IO_TIMEOUT;

/// Read/write half that can be cloned into a reader thread + writer handle.
pub(crate) trait AppBytePort: Read + Write + Send + 'static {
    fn try_clone_box(&self) -> io::Result<Box<dyn AppBytePort>>;
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()>;
}

struct UsbPort(Box<dyn SerialPort>);

impl Read for UsbPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for UsbPort {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl AppBytePort for UsbPort {
    fn try_clone_box(&self) -> io::Result<Box<dyn AppBytePort>> {
        self.0
            .try_clone()
            .map(|p| Box::new(UsbPort(p)) as Box<dyn AppBytePort>)
            .map_err(|e| io::Error::other(e.to_string()))
    }

    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.0
            .set_timeout(timeout)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

/// Open a host USB serial port as an [`AppBytePort`].
pub(crate) fn open_usb(name: &str, baud: u32) -> io::Result<Box<dyn AppBytePort>> {
    let port = serialport::new(name, baud)
        .timeout(PORT_IO_TIMEOUT)
        .open()
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(Box::new(UsbPort(port)))
}
