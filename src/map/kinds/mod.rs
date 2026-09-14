//! Map kinds: HASH / ARRAY / optional RINGBUF stub.

pub(crate) mod array;
pub(crate) mod hash;
#[cfg(feature = "ringbuf")]
pub(crate) mod ringbuf;
