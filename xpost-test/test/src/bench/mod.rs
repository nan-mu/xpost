//! Overnight / formal benchmarks.

pub mod all;
pub mod drop;
pub mod drop_common;
pub mod drop_hal;
pub mod drop_xdp;
pub mod dut_ready;
pub mod echo;
pub mod flash;
pub mod hping;
pub mod just_drop;
pub mod pg_dsn;
pub mod rate_common;
pub mod rate_loss;
pub mod rate_rtt;
pub mod reset;
pub mod stop;
pub mod udp_blast;
pub mod wl_common;
pub mod wl_loss;
pub mod wl_rtt;

pub use all::run_all as run_all_benches;
pub use drop_hal::run_drop_hal;
pub use drop_xdp::run_drop_xdp;
pub use echo::run_echo;
pub use just_drop::{JustDropOpts, run_just_drop};
pub use rate_loss::run_rate_loss;
pub use rate_rtt::run_rate_rtt;
pub use wl_loss::run_wl_loss;
pub use wl_rtt::run_wl_rtt;
