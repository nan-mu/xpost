//! XDP runtime and the Embassy driver injection point.
//!
//! The public surface is deliberately small. Slot management and RX execution
//! live in `runtime`; token and DMA handling live in `device`.

mod device;
mod runtime;

pub use device::{Device, ZcTx};
pub use runtime::{Error, Slot, XdpRt, XDP_RT};

pub(crate) use runtime::{execute, is_leased};
