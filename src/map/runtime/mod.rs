//! Map runtime singleton, talc pool, and name table.

pub(crate) mod lock;
pub(crate) mod names;
pub(crate) mod pool;

mod rt;
pub use lock::{MapRtGuard, RtMutex, RtMutexGuard};
pub use rt::*;
