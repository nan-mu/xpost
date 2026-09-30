//! # `xpost-test` — TR3000 formal / functional harness
//!
//! This crate owns the **end-to-end test workflow**. Mac-side USB is owned by
//! `xpost-agent` (HTTP `/v1/*`); low-level serial types live in **`xpost-serial`**
//! (see that crate’s rustdoc for `Session` / `install_xdp` / `Sink` — API only).
//!
//! Binary name: **`xpost-test`**. Compile-time config:
//! [`config/default.toml`](../../config/default.toml) +
//! [`just_tx.toml`](../../config/just_tx.toml) +
//! [`just_drop.toml`](../../config/just_drop.toml). Knobs: [`config`].
//!
//! # Formal performance path (primary)
//!
//! Always on TR3000, **no env / no CLI flags**:
//!
//! ```text
//! Mac (USB ESP)                         TR3000                         Postgres
//! cargo xtask agent                     ./xpost-test                   formal.*
//!   --listen 0.0.0.0:9240                 │                              ▲
//!   /health /v1/* /mcp                    │ FlashProfile / counters      │
//!         ▲                               │ request / probe              │
//!         └──────── HTTP [agent] ─────────┘                              │
//!                     RAW UDP stim + TC stamps ──► DUT ──► counters ─────┘
//! ```
//!
//! 1. Mac: `cargo xtask agent --listen 0.0.0.0:9240 --no-monitor`
//! 2. Edit `config/default.toml` `run = ["just_drop"]` and/or `["just_tx"]`
//!    (steps are fixed in Rust; TOML only selects experiments + knobs).
//! 3. Rebuild → scp → on TR3000: `./xpost-test`
//! 4. Analysis on Mac: `benchmark/` notebooks (`just_drop.ipynb` / `just_tx.ipynb`),
//!    kernel `xpost-benchmark` — see `benchmark.utils` package docstring.
//!
//! | `run` | Role | Tables |
//! |-------|------|--------|
//! | `just_drop` | HAL + XDP; kinds `c` full-rate + `a`/`g`/`t` fixed pps | `just_drop_*` |
//! | `just_tx` | HAL + XDP sweep → ~5% loss working point + RTT | `just_tx_*` |
//! | `dev` | Debug XDP TX grid only; **no DB** | — |
//!
//! Agent profile flash builds firmware + optional XDP ELF from `profiles.toml`.
//! Serial read face on DUT: `counters` / `agg` / `gauge` / `trace` / `perf clear`
//! (esp-radio only **calls** `EspRadioTrace`; storage is xpost `ext::perf`).
//!
//! # Topology
//!
//! ```text
//! 主路由 (DHCP/GW)
//!         │
//! Mac Mini / TR3000 / ESP 同网段
//!         │
//!    TR3000 ──2.4G── ESP32-C3 STA
//!      RAW + TC; agent on Mac flashes DUT
//! ```
//!
//! - Deploy **`xpost-test`** to TR3000; do not ssh the gateway for harness runs.
//! - Mac flashes via agent; does not own the stimulus path.
//! - DUT Wi-Fi credentials: `examples/just_drop/.env` (and HAL / just_tx_hal).
//!
//! # Stimulus contract (formal + suite)
//!
//! | Item | Convention |
//! |------|------------|
//! | Stim | UDP `dst = ESP_IP`, `dport` echo port (default 8080) |
//! | Payload | `FWSTIM` + `u64` LE `seq` |
//! | Timestamps | TC egress → T0; matching echo/bounce → T1 |
//!
//! # Build / deploy
//!
//! Container toolchain under `xpost-test/` (see `rust-toolchain.toml`):
//!
//! ```bash
//! cd xpost-test && cargo build -p test --release
//! scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
//! ```
//!
//! # Functional suite / legacy benches (secondary)
//!
//! - `./xpost-test suite` — fixed `test0`
//! - Hidden subcommands (`drop-xdp`, `echo`, `wl-*`, `rate-*`, `serve`, …): older
//!   profiles / UDP `:8081` control plane; not the formal PPS A/B path.
//! - Protocol matrices for those benches remain below for maintainers.
//!
//! ## `firewall_echo` / `xdp_forward_echo` / `fw_hash_test` / `hot_load_ctrl`
//!
//! See historical pass criteria in this module’s remainder (suite assets under
//! `test/assets/`, schemas under `sql/`). Prefer formal `just_*` for performance.
//!
//! ## Router deps (TR3000)
//!
//! ```bash
//! opkg update && opkg install kmod-sched-bpf kmod-sched-core ip-full
//! ip link   # confirm probe.iface (often br-lan)
//! ```
//!
//! ## Precision
//!
//! Deltas use TC `bpf_ktime_get_ns` only. Results are over-the-air RTT/PPS as seen
//! from TR3000 — relative comparisons, not on-chip cycle counts.

#![allow(dead_code)]

/// See module-level docs (workflow hub for `xpost-test`).
pub struct Overview;
