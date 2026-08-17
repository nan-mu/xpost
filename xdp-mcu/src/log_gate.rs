//! Boot / datapath logging macros.

/// Boot / one-shot logs.
#[macro_export]
macro_rules! init_log {
    ($($arg:tt)*) => {{
        log::info!($($arg)*);
    }};
}

/// Hot-swap / timing metrics.
#[macro_export]
macro_rules! bench_log {
    ($($arg:tt)*) => {{
        log::info!($($arg)*);
    }};
}

/// High-frequency datapath / helper / per-request logs (`ESP_LOG=debug`).
#[macro_export]
macro_rules! trace_log {
    ($($arg:tt)*) => {{
        log::debug!($($arg)*);
    }};
}
