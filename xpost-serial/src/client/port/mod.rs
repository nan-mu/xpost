//! USB serial port identity + exclusive gate for App UART vs espflash download.
//!
//! App I/O lives in [`crate::client::serial`] (`Uart` + `Log`). This module only
//! resolves the device and serializes opens (idf-monitor pause-before-flash style).

mod download;

pub use download::Downloader;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serialport::{SerialPortType, UsbPortInfo};

use crate::client::error::{Error, Result};
use crate::client::BAUD;
use crate::r#type::ops::Chip;

/// Resolved USB serial endpoint (path + identity). No open fd.
#[derive(Debug)]
pub struct Port {
    name: String,
    usb: UsbPortInfo,
    baud: u32,
    chip: Chip,
    /// Serialize App vs download opens (OS exclusive lock).
    gate: Mutex<()>,
}

impl Port {
    pub fn resolve(port: Option<&Path>, chip: Chip) -> Result<Self> {
        let (name, usb) = resolve_port(port)?;
        Ok(Self {
            name,
            usb,
            baud: BAUD,
            chip,
            gate: Mutex::new(()),
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

    /// Hold the exclusive gate while `f` runs (App UART or any non-download use).
    pub fn with_gate<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        let _gate = self
            .gate
            .lock()
            .map_err(|_| Error::Msg("port gate poisoned".into()))?;
        f()
    }

    /// Open download-mode flasher, run `f` (usually ends in [`Downloader::reset_to_app`]).
    pub fn with_download<R>(&self, f: impl FnOnce(Downloader) -> Result<R>) -> Result<R> {
        let _gate = self
            .gate
            .lock()
            .map_err(|_| Error::Msg("port gate poisoned".into()))?;
        let dl = Downloader::connect(&self.name, &self.usb, self.chip, self.baud)?;
        f(dl)
    }
}

pub(crate) fn tempfile_path() -> Result<PathBuf> {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "xpost-serial-readback-{}-{}.bin",
        std::process::id(),
        std::time::Instant::now().elapsed().as_nanos()
    ));
    Ok(p)
}

pub(crate) fn resolve_port(port: Option<&Path>) -> Result<(String, UsbPortInfo)> {
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
        0 => Err(Error::Msg("no USB serial ports found".into())),
        1 => Ok(unique.into_iter().next().unwrap()),
        _ => Err(Error::Msg(format!(
            "multiple USB serial ports; pass --port explicitly: {}",
            unique
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}
