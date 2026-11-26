//! Download-mode session via espflash (open for one flash op chain).

use std::fs;

use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
use espflash::flasher::{FlashData, FlashSettings, Flasher};
use espflash::image_format::ImageFormat;
use espflash::image_format::idf::IdfBootloaderFormat;
use espflash::target::{Chip as EspChip, DefaultProgressCallback, XtalFrequency};
use log::debug;
use serialport::UsbPortInfo;

use crate::client::ops::Chip;
use crate::client::port::tempfile_path;
use crate::client::{BAUD, READ_BLOCK, READ_MAX_IN_FLIGHT, RESET_TO_APP_SETTLE};
use std::io;

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

pub(crate) fn firmware_image(
    elf: &[u8],
    chip: EspChip,
    xtal: XtalFrequency,
) -> io::Result<ImageFormat<'_>> {
    let flash_data = FlashData::new(FlashSettings::default(), 0, None, chip, xtal);
    let idf = IdfBootloaderFormat::new(elf, &flash_data, None, None, None, None).map_err(ext)?;
    Ok(ImageFormat::from(idf))
}

/// Download-mode flasher. Drop releases the USB port.
pub struct Downloader {
    /// `None` only while reconnecting (must drop the old exclusive flock first).
    flasher: Option<Flasher>,
    name: String,
    usb: UsbPortInfo,
    chip: Chip,
    baud: u32,
    after_write: ResetAfterOperation,
}

impl Downloader {
    pub(crate) fn connect(
        name: &str,
        usb: &UsbPortInfo,
        chip: Chip,
        baud: u32,
        keep_stub: bool,
    ) -> io::Result<Self> {
        let after_write = if keep_stub {
            ResetAfterOperation::NoResetNoStub
        } else {
            ResetAfterOperation::HardReset
        };
        let flasher = connect_flasher(name, usb, chip, baud, after_write)?;
        Ok(Self {
            flasher: Some(flasher),
            name: name.to_string(),
            usb: usb.clone(),
            chip,
            baud,
            after_write,
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
            self.after_write,
        )?);
        Ok(())
    }

    pub(crate) fn application_segment(
        &mut self,
        firmware_elf: &[u8],
    ) -> io::Result<(u32, Vec<u8>)> {
        let (chip, xtal) = {
            let f = self.flasher()?;
            let chip = f.chip();
            let xtal = chip.xtal_frequency(f.connection()).map_err(ext)?;
            (chip, xtal)
        };
        let mut segments = firmware_image(firmware_elf, chip, xtal)?.ota_segments();
        if segments.len() != 1 {
            return Err(io::Error::other("expected one application segment"));
        }
        let segment = segments.pop().unwrap();
        Ok((segment.addr, segment.data.into_owned()))
    }

    /// Flash app ELF bytes, then overwrite partition table at `PART_TABLE_OFFSET`.
    pub fn flash_firmware(&mut self, firmware_elf: &[u8], part_bin: &[u8]) -> io::Result<()> {
        let (chip, xtal) = {
            let f = self.flasher()?;
            let chip = f.chip();
            let xtal = chip.xtal_frequency(f.connection()).map_err(ext)?;
            (chip, xtal)
        };
        let format = firmware_image(firmware_elf, chip, xtal)?;
        self.flasher()?
            .load_image_to_flash(&mut DefaultProgressCallback, format)
            .map_err(ext)?;

        self.reconnect()?;
        self.write_bin_verified(PART_TABLE_OFFSET, part_bin)?;
        Ok(())
    }

    /// Write and read back an XDP image on the attested download connection.
    pub(crate) fn write_xdp_verified(&mut self, addr: u32, data: &[u8]) -> io::Result<()> {
        self.flasher()?
            .write_bin_to_flash(addr, data, &mut DefaultProgressCallback)
            .map_err(ext)?;
        let read = self.read_flash(addr, data.len() as u32)?;
        if read != data {
            return Err(io::Error::other("XDP image differs on readback"));
        }
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
        let read =
            fs::read(&tmp).map_err(|e| std::io::Error::other(format!("readback file: {e}")))?;
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

    pub(crate) fn board_info(&mut self) -> io::Result<espflash::flasher::DeviceInfo> {
        self.flasher()?.device_info().map_err(ext)
    }

    /// Hard-reset to app and release the port.
    pub fn reset_to_app(mut self) -> io::Result<()> {
        let result = if let Some(mut flasher) = self.flasher.take() {
            flasher.connection().reset().map_err(ext)
        } else {
            Ok(())
        };
        std::thread::sleep(RESET_TO_APP_SETTLE);
        result
    }
}

fn connect_flasher(
    name: &str,
    usb: &UsbPortInfo,
    chip: Chip,
    baud: u32,
    after_write: ResetAfterOperation,
) -> io::Result<Flasher> {
    debug!("opening serial {name} for download");
    let serial = serialport::new(name, BAUD)
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(ext)?;
    // ESP32-C3 USB-JTAG-Serial needs UsbReset; DefaultReset is flaky after long
    // App sessions and breaks the subsequent XDP table write with a bootloader error.
    let before = match chip {
        Chip::Esp32c3 => ResetBeforeOperation::UsbReset,
    };
    let connection = Connection::new(serial, usb.clone(), after_write, before, baud);
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // ELF32 RISC-V executable with one allocated .text section in ESP32-C3 RAM.
    pub(crate) fn riscv_elf() -> Vec<u8> {
        let mut elf = vec![0u8; 0x100 + 3 * 40];
        elf[..16].copy_from_slice(b"\x7fELF\x01\x01\x01\0\0\0\0\0\0\0\0\0");
        fn u16le(b: &mut [u8], at: usize, n: u16) {
            b[at..at + 2].copy_from_slice(&n.to_le_bytes());
        }
        fn u32le(b: &mut [u8], at: usize, n: u32) {
            b[at..at + 4].copy_from_slice(&n.to_le_bytes());
        }
        u16le(&mut elf, 16, 2);
        u16le(&mut elf, 18, 243);
        u32le(&mut elf, 20, 1);
        u32le(&mut elf, 24, 0x40380000);
        u32le(&mut elf, 32, 0x100);
        u16le(&mut elf, 40, 52);
        u16le(&mut elf, 46, 40);
        u16le(&mut elf, 48, 3);
        u16le(&mut elf, 50, 2);
        elf[0x80..0x84].copy_from_slice(&[0x13, 0, 0, 0]);
        elf[0x84..0x95].copy_from_slice(b"\0.text\0.shstrtab\0");
        let text = 0x100 + 40;
        u32le(&mut elf, text, 1);
        u32le(&mut elf, text + 4, 1);
        u32le(&mut elf, text + 8, 6);
        u32le(&mut elf, text + 12, 0x40380000);
        u32le(&mut elf, text + 16, 0x80);
        u32le(&mut elf, text + 20, 4);
        u32le(&mut elf, text + 32, 4);
        let strings = text + 40;
        u32le(&mut elf, strings, 7);
        u32le(&mut elf, strings + 4, 3);
        u32le(&mut elf, strings + 16, 0x84);
        u32le(&mut elf, strings + 20, 17);
        u32le(&mut elf, strings + 32, 1);
        elf
    }

    #[test]
    fn generated_application_matches_flash_image() {
        let elf = riscv_elf();
        let image = || firmware_image(&elf, EspChip::Esp32c3, XtalFrequency::_40Mhz).unwrap();
        let ota = image().ota_segments();
        assert_eq!(ota.len(), 1);
        assert_eq!(ota[0].addr, 0x10000);
        assert!(!ota[0].data.is_empty());
        assert!(
            image()
                .flash_segments()
                .iter()
                .any(|s| s.addr == ota[0].addr && s.data == ota[0].data)
        );
        assert!(firmware_image(b"corrupt", EspChip::Esp32c3, XtalFrequency::_40Mhz).is_err());
    }
}
