//! Process-wide XDP runtime state and fixed capacity / budget limits.
//!
//! Ingress and board hooks should pull numeric limits from here; atomics for
//! lease/flight live alongside them.

use embassy_sync::once_lock::OnceLock;
use portable_atomic::{AtomicBool, AtomicUsize};

use crate::{RtMutex, xdp::XdpRt};

// —— runtime table capacities ——

pub(crate) const MAX_SLOTS: usize = 4;
pub(crate) const MAX_MAP_REFS: usize = 16;
pub(crate) const MAX_IMAGES: usize = 8;
/// MapDescriptorV0 / AOT bind entries per Compact ELF image.
pub(crate) const MAX_REQUIRED_MAPS: usize = 8;
/// [`crate::xdp::map::VmrTable`] / LinearMap capacity.
pub(crate) const MAX_VMR_BINDS: usize = 16;

// —— soft stack canary (debug / hwtest; does not prove N2/N3) ——

/// Words painted below `sp` before `jalr` into the XDP image.
pub(crate) const STACK_CANARY_WORDS: usize = 8;
/// Sentinel value for the soft shared-stack canary.
pub(crate) const STACK_CANARY_MAGIC: u32 = 0xA07_CAFE;
/// Bytes left unused between `sp` and the canary band (args / align gap).
pub(crate) const STACK_CANARY_GAP_BYTES: usize = 16;

// —— lease / flight atomics ——

pub static XDP_RT: OnceLock<RtMutex<XdpRt>> = OnceLock::new();
pub(crate) static ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static PRIMARY_ENTRY: AtomicUsize = AtomicUsize::new(0);
pub(crate) static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Hot-update swing. Ingress drops a new flight; `ACTIVE` stays true.
///
/// `XdpRt::switch` stores this, waits until `IN_FLIGHT` is 0, then publishes
/// the new `PRIMARY_ENTRY`. The RX callback must not spin on this flag.
pub(crate) static PAUSE: AtomicBool = AtomicBool::new(false);
