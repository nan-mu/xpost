//! bpf.toml boot-image loading via [`crate::xdp::XdpBuilder`].

use crate::xdp::error::XdpError;
use crate::xdp::meta::XdpLocation;
use crate::xdp::{XdpRt, XdpTableId};

pub(crate) struct BpfTomlImage {
    pub name: &'static str,
    pub table_id: XdpTableId,
    pub link_addr: usize,
    pub helper_address: usize,
    pub abi_version: u32,
    pub required_maps: &'static [&'static str],
    pub bytes: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/xdp_bpf.rs"));

pub(crate) fn load(rt: &mut XdpRt) -> Result<(), XdpError> {
    for image in BOOT_IMAGES {
        let id = rt.push_at_table(
            image.name,
            image.bytes,
            image.table_id,
            image.link_addr,
            image.helper_address,
            image.abi_version,
            image.required_maps,
        )?;
        let slot = rt.slot_for(image.link_addr)?;
        rt.r#move(id, XdpLocation::Slot { index: slot })?;
    }
    Ok(())
}
