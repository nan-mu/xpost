//! Map runtime singleton, talc pool, and name table.

pub(crate) mod names;
pub(crate) mod pool;

mod rt;

pub use crate::{RtMutex, RtMutexGuard};
/// Locked [`crate::map::MapRt`].
pub type MapRtGuard<'a> = RtMutexGuard<'a, crate::map::MapRt>;
pub use rt::*;
