//! Map runtime singleton, talc pool, name table, and bpf.toml boot.

#[cfg(feature = "from-bpf-toml")]
pub mod boot;
pub mod lock;
pub mod names;
pub mod pool;

mod rt;
pub use lock::{MapRtGuard, RtMutex, RtMutexGuard};
pub use rt::*;
