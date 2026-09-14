//! Process-wide XDP runtime state and fixed capacity / budget limits.
//!
//! Ingress and board hooks should pull numeric limits from here; atomics for
//! lease/flight live alongside them.

use embassy_sync::once_lock::OnceLock;
use portable_atomic::{AtomicBool, AtomicUsize};

use crate::xdp::{XdpRt, lock::RtMutex};

// —— runtime table capacities ——

pub(crate) const MAX_SLOTS: usize = 4;
pub(crate) const MAX_MAP_REFS: usize = 16;
pub(crate) const MAX_IMAGES: usize = 8;
pub(crate) const MAX_VMR_BINDS: usize = 16;

// —— shared-stack budget (E3 docs / placeholders; not enforced at runtime) ——

/// eBPF verifier stack budget (prevail `--stack-size` default).
pub(crate) const BPF_STACK_BUDGET: usize = 512;
/// Extra shared-stack room assumed for synchronous helpers.
pub(crate) const HELPER_STACK_BUDGET: usize = 512;
/// Conservative XDP image + helper peak on the shared caller stack.
pub(crate) const XDP_STACK_PEAK_BUDGET: usize = BPF_STACK_BUDGET + HELPER_STACK_BUDGET;

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
