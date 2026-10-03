//! Default / firmware-first boot (`xdp-firmware-first`).
//!
//! Register an XdpTable entity with [`crate::xdp::XdpBuilder::for_firmware_image`],
//! then [`crate::xdp::XdpRt::move`] into a slot and [`crate::xdp::XdpRt::lease`].

pub use crate::xdp::{XdpBuilder, XdpRt};
