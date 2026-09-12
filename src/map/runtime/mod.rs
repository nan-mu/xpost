//! Map runtime singleton, talc pool, and name table.

pub mod lock;
pub mod names;
pub mod pool;

mod rt;
pub use lock::{MapRtGuard, RtMutex, RtMutexGuard};
pub use rt::*;
