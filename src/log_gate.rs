//! Logging gates for the `benchmark` Cargo feature.
//!
//! - `init_log!` — boot / one-shot messages (always on; WARN in benchmark).
//! - `bench_log!` — timing / hot-swap metrics (always on; WARN in benchmark).
//! - `trace_log!` — per-packet / helper / ctrl chatter (off in benchmark).

/// Boot / one-shot logs.
#[macro_export]
macro_rules! init_log {
    ($($arg:tt)*) => {{
        #[cfg(feature = "benchmark")]
        log::warn!($($arg)*);
        #[cfg(not(feature = "benchmark"))]
        log::info!($($arg)*);
    }};
}

/// Hot-swap / timing metrics.
#[macro_export]
macro_rules! bench_log {
    ($($arg:tt)*) => {{
        #[cfg(feature = "benchmark")]
        log::warn!($($arg)*);
        #[cfg(not(feature = "benchmark"))]
        log::info!($($arg)*);
    }};
}

/// High-frequency datapath / helper / per-request logs.
#[macro_export]
macro_rules! trace_log {
    ($($arg:tt)*) => {{
        #[cfg(not(feature = "benchmark"))]
        log::info!($($arg)*);
    }};
}

pub fn init_logger() {
    #[cfg(feature = "benchmark")]
    esp_println::logger::init_logger(log::LevelFilter::Warn);
    #[cfg(not(feature = "benchmark"))]
    esp_println::logger::init_logger_from_env();
}
