//! Trace backends under `ext`.
//!
//! | Module | Feature | Role |
//! |--------|---------|------|
//! | [`counter`] | `trace-counter` | atomics + [`Observe`](xpost_trait::Observe) |
//! | [`timestamp`] | `trace-timestamp` | helper enter/leave ring |
//!
//! The attach / public singleton is always [`crate::XDP_PROG`] in [`crate::trace`].

#[cfg(feature = "trace-counter")]
pub mod counter;

#[cfg(feature = "trace-timestamp")]
pub mod timestamp;
