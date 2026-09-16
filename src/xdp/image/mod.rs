//! XDP image metadata and registration factory.

mod builder;
pub(crate) mod elf;
mod meta;

pub use builder::{Complete, Incomplete, XdpBuilder, XdpMaps};
pub use meta::XdpLocation;
pub(crate) use meta::XdpMeta;
