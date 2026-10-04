//! Download-mode session via espflash (open for one flash op chain).

use std::fs;

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::ImageFormat;
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::{Chip as EspChip, DefaultProgressCallback};
use log::debug;
use serialport::UsbPortInfo;

use std::io;
use crate::client::ops::Chip;
use crate::client::port::tempfile_path;
use crate::client::{BAUD, READ_BLOCK, READ_MAX_IN_FLIGHT, RESET_TO_APP_SETTLE};

/// Default ESP-IDF partition-table flash offset (ESP32-C3).
pub const PART_TABLE_OFFSET: u32 = 0x8000;

impl Chip {
    pub(crate) fn as_espflash(self) -> EspChip {
        match self {
            Self::Esp32c3 => EspChip::Esp32c3,
        }
    }
}

fn ext(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

/// Download-mode flasher. Drop releases the USB port.
pub struct Downloader {
    /// `None` only while reconnecting (must drop the old exclusive flock first).
    flasher: Option<Flasher>,
    name: String,
    usb: UsbPortInfo,
    chip: Chip,
    baud: u32,
}

impl Downloader {
    pub(crate) fn connect(name: &str, usb: &UsbPortInfo, chip: Chip, baud: u32) -> io::Result<Self> {
        let flasher = connect_flasher(name, usb, chip, baud)?;
        Ok(Self {
            flasher: Some(flasher),
            name: name.to_string(),
            usb: usb.clone(),
            chip,
            baud,
        })
    }

    fn flasher(&mut self) -> io::Result<&mut Flasher> {
        self.flasher
            .as_mut()
            .ok_or_else(|| io::Error::other("download flasher not connected"))
    }

    pub fn reconnect(&mut self) -> io::Result<()> {
        // Drop the live connection before opening again. `mem::replace` with a
        // fresh `connect_flasher` would open while the old exclusive flock is
        // still held → EBUSY on macOS USB-JTAG.
        self.flasher = None;
        self.flasher = Some(connect_flasher(
            &self.name,
            &self.usb,
            self.chip,
            self.baud,
        )?);
        Ok(())
    }

    /// Flash app ELF bytes, then overwrite partition table at `PART_TABLE_OFFSET`.
    pub fn flash_firmware(&mut self, firmware_elf: &[u8], part_bin: &[u8]) -> io::Result<()> {
        let (chip, xtal) = {
            let f = self.flasher()?;
            let chip = f.chip();
            let xtal = chip.xtal_frequency(f.connection()).map_err(ext)?;
            (chip, xtal)
        };
        let flash_data = FlashData::new(FlashSettings::default(), 0, None, chip, xtal);
        let idf = IdfBootloaderFormat::new(firmware_elf, &flash_data, None, None, None, None)
            .map_err(ext)?;
        let format = ImageFormat::from(idf);
        self.flasher()?
            .load_image_to_flash(&mut DefaultProgressCallback, format)
            .map_err(ext)?;

        self.reconnect()?;
        self.write_bin_verified(PART_TABLE_OFFSET, part_bin)?;
        Ok(())
    }

    pub fn write_bin_verified(&mut self, addr: u32, data: &[u8]) -> io::Result<()> {
        self.flasher()?
            .write_bin_to_flash(addr, data, &mut DefaultProgressCallback)
            .map_err(ext)?;
        self.reconnect()?;
        let tmp = tempfile_path()?;
        let len = data.len() as u32;
        self.flasher()?
            .read_flash(addr, len, READ_BLOCK, READ_MAX_IN_FLIGHT, tmp.clone())
            .map_err(ext)?;
        let read = fs::read(&tmp).map_err(|e| std::io::Error::other(format!("readback file: {e}")))?;
        let _ = fs::remove_file(&tmp);
        if read.len() < data.len() {
            return Err(std::io::Error::other(format!(
                "verify length mismatch: wrote {}, read {}",
                data.len(),
                read.len()
            )));
        }
        for (i, (&want, &got)) in data.iter().zip(read.iter()).enumerate() {
            if want != got {
                return Err(std::io::Error::other(format!(
                    "verify mismatch at offset {i:#x}: wrote {want:#04x}, read {got:#04x}"
                )));
            }
        }
        Ok(())
    }

    pub fn read_flash(&mut self, addr: u32, len: u32) -> io::Result<Vec<u8>> {
        let tmp = tempfile_path()?;
        self.flasher()?
            .read_flash(addr, len, READ_BLOCK, READ_MAX_IN_FLIGHT, tmp.clone())
            .map_err(ext)?;
        let data = fs::read(&tmp).map_err(|e| std::io::Error::other(format!("read flash: {e}")))?;
        let _ = fs::remove_file(&tmp);
        if data.len() < len as usize {
            return Err(std::io::Error::other(format!(
                "short flash read {} < {len}",
                data.len()
            )));
        }
        Ok(data[..len as usize].to_vec())
    }

    pub fn erase_region(&mut self, addr: u32, len: u32) -> io::Result<()> {
        self.flasher()?.erase_region(addr, len).map_err(ext)?;
        Ok(())
    }

    /// Hard-reset to app and release the port.
    pub fn reset_to_app(mut self) -> io::Result<()> {
        if let Some(mut flasher) = self.flasher.take() {
            let chip = flasher.chip();
            let _ = flasher.connection().reset_after(true, chip);
            drop(flasher);
        }
        std::thread::sleep(RESET_TO_APP_SETTLE);
        Ok(())
    }
}

fn connect_flasher(name: &str, usb: &UsbPortInfo, chip: Chip, baud: u32) -> io::Result<Flasher> {
    debug!("opening serial {name} for download");
    let serial = serialport::new(name, BAUD)
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(ext)?;
    let connection = Connection::new(
        serial,
        usb.clone(),
        ResetAfterOperation::HardReset,
        ResetBeforeOperation::DefaultReset,
        baud,
    );
    let flasher = Flasher::connect(
        connection,
        true,
        false,
        false,
        Some(chip.as_espflash()),
        Some(baud),
    )
    .map_err(ext)?;
    Ok(flasher)
}
