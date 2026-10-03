//! USB serial port: identity, exclusive gate, App [`Uart`], espflash download.
//!
//! Sessions from [`Port::open_uart`] / [`Port::open_download`] hold the fd until
//! drop. App `serial::request` runs under `tokio::select!`; interrupt/timeout
//! returns early so the session drops and flash can open download.

mod byte_port;
mod download;
mod link;
mod uart;

#[cfg(all(test, feature = "dev"))]
pub mod memory;

pub use download::Downloader;
pub(crate) use link::AppLink;
pub use uart::{Command, Interrupt, Uart};

#[cfg(all(test, feature = "dev"))]
pub use memory::{MemoryClient, MemoryPair, MemoryServer};

use crate::client::record::SharedRecord;

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use std::io;
use serialport::{SerialPortType, UsbPortInfo};

use crate::client::BAUD;
use crate::client::ops::Chip;

/// Resolved USB serial endpoint. No open fd until [`Self::open_uart`] /
/// [`Self::open_download`].
#[derive(Debug)]
pub struct Port {
    name: String,
    usb: UsbPortInfo,
    baud: u32,
    chip: Chip,
    gate: Mutex<()>,
    interrupt: Interrupt,
}

/// App UART session: holds exclusive gate + fd until drop.
pub struct UartSession<'a> {
    _gate: std::sync::MutexGuard<'a, ()>,
    uart: Uart,
}

impl Deref for UartSession<'_> {
    type Target = Uart;
    fn deref(&self) -> &Uart {
        &self.uart
    }
}

impl DerefMut for UartSession<'_> {
    fn deref_mut(&mut self) -> &mut Uart {
        &mut self.uart
    }
}

/// Download-mode session: holds exclusive gate + fd until drop.
pub struct DownloadSession<'a> {
    _gate: std::sync::MutexGuard<'a, ()>,
    dl: Downloader,
}

impl Deref for DownloadSession<'_> {
    type Target = Downloader;
    fn deref(&self) -> &Downloader {
        &self.dl
    }
}

impl DerefMut for DownloadSession<'_> {
    fn deref_mut(&mut self) -> &mut Downloader {
        &mut self.dl
    }
}

impl DownloadSession<'_> {
    /// Hard-reset to app, drop the download fd, then release the gate.
    pub fn reset_to_app(self) -> io::Result<()> {
        let Self { _gate, dl } = self;
        dl.reset_to_app()
    }
}

impl Port {
    pub fn resolve(port: Option<&Path>, chip: Chip) -> io::Result<Self> {
        let (name, usb) = resolve_port(port)?;
        Ok(Self {
            name,
            usb,
            baud: BAUD,
            chip,
            gate: Mutex::new(()),
            interrupt: Interrupt::new(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn baud(&self) -> u32 {
        self.baud
    }

    pub fn chip(&self) -> Chip {
        self.chip
    }

    pub fn interrupt(&self) -> &Interrupt {
        &self.interrupt
    }

    /// Occupy App UART fd until the returned session is dropped.
    ///
    /// Spawns a reader that continuously appends framed UART into `record`.
    /// Clears a stale interrupt first. Race work against [`Interrupt::wait`] /
    /// a deadline so drop releases the gate for [`Self::open_download`].
    pub(crate) fn open_uart(&self, record: SharedRecord) -> io::Result<UartSession<'_>> {
        self.interrupt.clear();
        let gate = self
            .gate
            .lock()
            .map_err(|_| std::io::Error::other("port gate poisoned"))?;
        let uart = Uart::open_usb(&self.name, self.baud, record)?;
        Ok(UartSession { _gate: gate, uart })
    }

    /// Occupy download-mode fd until the returned session is dropped.
    ///
    /// Blocks on the gate until any App [`UartSession`] is dropped (after
    /// interrupt/timeout abort).
    pub(crate) fn open_download(&self) -> io::Result<DownloadSession<'_>> {
        let gate = self
            .gate
            .lock()
            .map_err(|_| std::io::Error::other("port gate poisoned"))?;
        let dl = Downloader::connect(&self.name, &self.usb, self.chip, self.baud)?;
        Ok(DownloadSession { _gate: gate, dl })
    }
}

pub(crate) fn tempfile_path() -> io::Result<PathBuf> {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "xpost-serial-readback-{}-{}.bin",
        std::process::id(),
        std::time::Instant::now().elapsed().as_nanos()
    ));
    Ok(p)
}

pub(crate) fn resolve_port(port: Option<&Path>) -> io::Result<(String, UsbPortInfo)> {
    let ports = serialport::available_ports().unwrap_or_default();
    if let Some(want) = port {
        let want_s = want.display().to_string();
        let canon = std::fs::canonicalize(want).ok();
        for p in &ports {
            if p.port_name == want_s
                || canon
                    .as_ref()
                    .and_then(|_c| std::fs::canonicalize(&p.port_name).ok())
                    .as_ref()
                    == canon.as_ref()
            {
                let usb = match &p.port_type {
                    SerialPortType::UsbPort(u) => u.clone(),
                    _ => UsbPortInfo {
                        vid: 0,
                        pid: 0,
                        serial_number: None,
                        manufacturer: None,
                        product: None,
                    },
                };
                return Ok((p.port_name.clone(), usb));
            }
        }
        return Ok((
            want_s,
            UsbPortInfo {
                vid: 0,
                pid: 0,
                serial_number: None,
                manufacturer: None,
                product: None,
            },
        ));
    }

    let usb_ports: Vec<_> = ports
        .into_iter()
        .filter(|p| matches!(p.port_type, SerialPortType::UsbPort(_)))
        .collect();
    let cu_only: Vec<_> = usb_ports
        .iter()
        .filter(|p| {
            let n = p.port_name.as_str();
            n.contains("/cu.") || n.starts_with("cu.")
        })
        .cloned()
        .collect();
    let usb_ports = if cu_only.is_empty() {
        usb_ports
    } else {
        cu_only
    };
    let mut unique: Vec<(String, UsbPortInfo)> = Vec::new();
    for p in &usb_ports {
        let usb = match &p.port_type {
            SerialPortType::UsbPort(u) => u.clone(),
            _ => continue,
        };
        let dup = unique.iter().any(|(_, u)| {
            u.vid == usb.vid && u.pid == usb.pid && u.serial_number == usb.serial_number
        });
        if !dup {
            unique.push((p.port_name.clone(), usb));
        }
    }
    match unique.len() {
        0 => Err(std::io::Error::other("no USB serial ports found")),
        1 => Ok(unique.into_iter().next().unwrap()),
        _ => Err(std::io::Error::other(format!(
            "multiple USB serial ports; pass --port explicitly: {}",
            unique
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}
