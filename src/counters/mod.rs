//! Process-wide diagnostic failure / success counters (Relaxed atomics).

pub mod egress;

pub use egress::{EGRESS, EgressStats};
