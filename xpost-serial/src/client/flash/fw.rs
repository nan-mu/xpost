use crate::client::{FirmwareExpectation, port::Port};
use sha2::{Digest, Sha256};
use std::io;

pub(crate) fn flash_fw(port: &Port, snapshot: &FirmwareExpectation) -> io::Result<()> {
    if <[u8; 32]>::from(Sha256::digest(&snapshot.elf)) != snapshot.sha256 {
        return Err(io::Error::other("firmware snapshot SHA256 mismatch"));
    }
    let mut dl = port.open_download()?;
    let result = dl.flash_firmware(&snapshot.elf, &snapshot.partition_table);
    let reset = dl.reset_to_app();
    result?;
    reset
}
