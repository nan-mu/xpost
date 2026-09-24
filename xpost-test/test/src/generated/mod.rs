//! Generated bpf.toml metadata (`cargo xtask gen-hot-meta`).

#[path = "hot_a_bpf.rs"]
mod hot_a_bpf;
#[path = "tail_call_bpf.rs"]
mod tail_call_bpf;

pub use hot_a_bpf::*;
pub mod tail_call {
    pub use super::tail_call_bpf::*;
}
