//! bpf.toml boot-image loading.

use super::XdpRt;
use super::error::XdpError;
use super::meta::{XdpArch, XdpLocation, XdpMeta};

pub(super) struct BpfTomlImage {
    pub table_id: super::XdpTableId,
    pub link_addr: usize,
    pub helper_address: usize,
    pub abi_version: u32,
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/xdp_bpf.rs"));

pub(super) fn load(rt: &mut XdpRt) -> Result<(), XdpError> {
    for image in BOOT_IMAGES {
        let id = rt.register_image_bytes(
            image.bytes,
            XdpMeta::generated(
                image.abi_version,
                XdpArch::Rv32Imc,
                image.helper_address,
                image.link_addr,
                heapless::Vec::new(),
                XdpLocation::Table(image.table_id),
            ),
            Some(image.table_id),
        )?;
        let slot = rt.slot_for(image.link_addr)?;
        rt.load(id, slot)?;
    }
    Ok(())
}
