//! Download-mode session via espflash (open for one flash op chain).

use std::fs;

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::image_format::ImageFormat;
use espflash::target::{Chip as EspChip, DefaultProgressCallback};
use log::debug;
use serialport::UsbPortInfo;

use crate::client::error::{Error, Result};
use crate::client::port::tempfile_path;
use crate::client::{BAUD, READ_BLOCK, READ_MAX_IN_FLIGHT, RESET_TO_APP_SETTLE};
use crate::r#type::ops::Chip;

/// Default ESP-IDF partition-table flash offset (ESP32-C3).
pub const PART_TABLE_OFFSET: u32 = 0x8000;

impl Chip {
    pub(crate) fn as_espflash(self) -> EspChip {
        match self {
            Self::Esp32c3 => EspChip::Esp32c3,
        }
    }
}

/// Download-mode flasher. Drop releases the USB port.
pub struct Downloader {
    flasher: Flasher,
    name: String,
    usb: UsbPortInfo,
    chip: Chip,
    baud: u32,
}

impl Downloader {
    pub(crate) fn connect(
        name: &str,
        usb: &UsbPortInfo,
        chip: Chip,
        baud: u32,
    ) -> Result<Self> {
        let flasher = connect_flasher(name, usb, chip, baud)?;
        Ok(Self {
            flasher,
            name: name.to_string(),
            usb: usb.clone(),
            chip,
            baud,
        })
    }

    pub fn reconnect(&mut self) -> Result<()> {
        drop(std::mem::replace(
            &mut self.flasher,
            connect_flasher(&self.name, &self.usb, self.chip, self.baud)?,
        ));
        Ok(())
    }

    /// Flash app ELF bytes, then overwrite partition table at [`PART_TABLE_OFFSET`].
    pub fn flash_firmware(&mut self, firmware_elf: &[u8], part_bin: &[u8]) -> Result<()> {
        let chip = self.flasher.chip();
        let xtal = chip.xtal_frequency(self.flasher.connection())?;
        let flash_data = FlashData::new(FlashSettings::default(), 0, None, chip, xtal);
        let idf = IdfBootloaderFormat::new(firmware_elf, &flash_data, None, None, None, None)?;
        let format = ImageFormat::from(idf);
        self.flasher
            .load_image_to_flash(&mut DefaultProgressCallback, format)?;

        self.reconnect()?;
        self.write_bin_verified(PART_TABLE_OFFSET, part_bin)?;
        Ok(())
    }

    pub fn write_bin_verified(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        self.flasher
            .write_bin_to_flash(addr, data, &mut DefaultProgressCallback)?;
        self.reconnect()?;
        let tmp = tempfile_path()?;
        let len = data.len() as u32;
        self.flasher
            .read_flash(addr, len, READ_BLOCK, READ_MAX_IN_FLIGHT, tmp.clone())?;
        let read = fs::read(&tmp).map_err(|e| Error::Msg(format!("readback file: {e}")))?;
        let _ = fs::remove_file(&tmp);
        if read.len() < data.len() {
            return Err(Error::VerifyLen {
                wrote: data.len(),
                read: read.len(),
            });
        }
        for (i, (&want, &got)) in data.iter().zip(read.iter()).enumerate() {
            if want != got {
                return Err(Error::VerifyMismatch {
                    offset: i,
                    want,
                    got,
                });
            }
        }
        Ok(())
    }

    pub fn read_flash(&mut self, addr: u32, len: u32) -> Result<Vec<u8>> {
        let tmp = tempfile_path()?;
        self.flasher
            .read_flash(addr, len, READ_BLOCK, READ_MAX_IN_FLIGHT, tmp.clone())?;
        let data = fs::read(&tmp).map_err(|e| Error::Msg(format!("read flash: {e}")))?;
        let _ = fs::remove_file(&tmp);
        if data.len() < len as usize {
            return Err(Error::Msg(format!(
                "short flash read {} < {len}",
                data.len()
            )));
        }
        Ok(data[..len as usize].to_vec())
    }

    pub fn erase_region(&mut self, addr: u32, len: u32) -> Result<()> {
        self.flasher.erase_region(addr, len)?;
        Ok(())
    }

    /// Hard-reset to app and release the port.
    pub fn reset_to_app(mut self) -> Result<()> {
        let chip = self.flasher.chip();
        let _ = self.flasher.connection().reset_after(true, chip);
        drop(self.flasher);
        std::thread::sleep(RESET_TO_APP_SETTLE);
        Ok(())
    }
}

fn connect_flasher(
    name: &str,
    usb: &UsbPortInfo,
    chip: Chip,
    baud: u32,
) -> Result<Flasher> {
    debug!("opening serial {name} for download");
    let serial = serialport::new(name, BAUD)
        .flow_control(serialport::FlowControl::None)
        .open_native()?;
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
    )?;
    Ok(flasher)
}
