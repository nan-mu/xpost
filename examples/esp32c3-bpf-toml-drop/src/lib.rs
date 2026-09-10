#![no_std]

extern crate alloc;

pub mod connection;
pub mod log_gate;
pub mod sta;

pub use xpost::{bench_log, init_log, trace_log};
