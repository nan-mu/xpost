//! Formal / flame exams driven by compile-time `config/`.

pub mod dev;
pub mod firewall;
pub mod just_drop;
pub mod just_tx;

pub use dev::run_dev;
pub use firewall::{FirewallOpts, run_firewall};
pub use just_drop::{JustDropPpsOpts, run_just_drop_pps};
pub use just_tx::{JustTxOpts, run_just_tx};
