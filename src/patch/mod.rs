pub mod ctx;
pub mod datapath;
mod helper;
pub mod map;
pub mod mem_map;
pub mod part;
pub mod prog_table;
pub mod program;
pub mod redirect;
pub(crate) mod sync;

#[cfg(fw_us_hw_selftest)]
pub mod hwtest;

pub use helper::helper_dispatcher;
pub use part::{load, read_slot, xdp};
pub use prog_table::SlotId;
pub use program::{
    ProgramError, ProgramId, XDP_SLOT_COUNT, XDP_SLOT_LEN, XdpInstallCmd, image as xdp_image,
    install as install_xdp, map_binds as xdp_map_binds, promote as promote_xdp,
    remove as remove_xdp, stage_raw_into as stage_raw_xdp, swap_hot as swap_hot_xdp,
};
