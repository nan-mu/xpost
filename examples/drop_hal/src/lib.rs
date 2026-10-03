#![no_std]

extern crate alloc;

pub mod connection;
pub mod drop_stats;
pub mod fw_cfg;
pub mod log_gate;
pub mod sta;

#[macro_export]
macro_rules! init_log {
    ($($t:tt)*) => {
        ::log::info!($($t)*);
    };
}

#[macro_export]
macro_rules! trace_log {
    ($($t:tt)*) => {
        ::log::trace!($($t)*);
    };
}
