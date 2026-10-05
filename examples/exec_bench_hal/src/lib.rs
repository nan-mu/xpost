#![no_std]

//! Native HAL twin of `exec_bench` (no `xpost` runtime).
//!
//! Re-export [`hal_perf`] for CAG report + App serial map wire helpers.

pub use hal_perf as cag;
pub use hal_perf::wire;
