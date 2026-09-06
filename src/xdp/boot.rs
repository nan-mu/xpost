//! bpf.toml boot-image loading.

use super::{XdpMeta, XdpRt};
use anyhow::Result;

pub(super) struct BpfTomlImage {
    pub table_id: super::XdpTableId,
    pub link_addr: usize,
    pub bytes: &'static [u8],
}
include!(concat!(env!("OUT_DIR"), "/xdp_bpf.rs"));

pub(super) fn load(rt: &mut XdpRt) -> Result<()> {
    for image in BOOT_IMAGES {
        let mut meta = XdpMeta::in_table(image.table_id, image.link_addr, image.bytes.len());
        rt.save(&mut meta, image.bytes)?;
        rt.load(&mut meta, rt.slot_for(image.link_addr)?)?;
    }
    Ok(())
}
