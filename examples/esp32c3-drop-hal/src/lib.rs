#![no_std]

#[macro_export]
macro_rules! init_log {
    ($($arg:tt)*) => { log::info!($($arg)*); };
}

pub mod connection;
pub mod drop_stats;
pub mod fw_cfg;
pub mod log_gate;
pub mod sta;
