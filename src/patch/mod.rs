mod helper;
mod part;
pub(crate) mod sync;
pub mod map;
pub mod mem_map;
pub mod ctx;
pub mod program;

#[cfg(feature = "hw-selftest")]
pub mod hwtest;

pub use helper::helper_dispatcher;
pub use part::{load, smoke_xdp, xdp};
pub use program::{
    image as xdp_image, install as install_xdp, map_binds as xdp_map_binds, remove as remove_xdp,
    ProgramError, ProgramId, XdpInstallCmd, XDP_SLOT_COUNT, XDP_SLOT_LEN,
};

pub fn smoke_multi_ctx() {
    #[cfg(feature = "hw-selftest")]
    hwtest::ctx_test::smoke_multi_ctx();
    #[cfg(not(feature = "hw-selftest"))]
    {
        // Production smoke: short frame through loaded AOT (no pass/fail counters).
        let mut frame = [
            0xAAu8, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x08, 0x00,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let _ = xdp(&mut frame);
    }
}
