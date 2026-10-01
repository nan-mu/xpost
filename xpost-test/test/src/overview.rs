//! # `xpost-test` — TR3000 formal / flame harness
//!
//! This crate owns the **end-to-end experiment workflow**. Mac-side USB is owned by
//! `xpost-agent` (HTTP `/v1/*`); low-level serial types live in **`xpost-serial`**.
//!
//! Binary name: **`xpost-test`**. Compile-time config only:
//! [`config/default.toml`](../../config/default.toml) +
//! [`just_tx.toml`](../../config/just_tx.toml) +
//! [`just_drop.toml`](../../config/just_drop.toml). Knobs: [`config`].
//!
//! # Sole entry
//!
//! Always on TR3000, **no env / no CLI flags / no subcommands**:
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
//! 2. Edit `config/default.toml` `run = ["just_drop"]` and/or `["just_tx"]` /
//!    `["dev"]` / `["formal-clear"]` (steps fixed in Rust; TOML selects experiments).
//! 3. Rebuild → scp → on TR3000: `./xpost-test`
//!
//! | `run` | Role | Tables |
//! |-------|------|--------|
//! | `just_drop` | `xdp_cag`/`hal` PPS grid + Array Map dump + `xdp_t` Trace | `exam` + `just_drop_*` + perf |
//! | `just_tx` | `xdp_cag`/`hal` pps grid RTT + `xdp_t` Trace | `exam` + `just_tx_*` (sent/ok; success RTT) + perf |
//! | `dev` | Flame Trace only: XDP × 100 pps × 2s × 1; report file; **no DB** | — |
//! | `formal-clear` | `TRUNCATE exam CASCADE` | wipe |
//!
//! Formal DDL: `sql/formal.sql` (shared) + `just_tx.sql` / `just_drop.sql`.
//! Open-run never DROP/TRUNCATE; wipe with `run = ["formal-clear"]`.
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
//! # Stimulus contract
//!
//! | Item | Convention |
//! |------|------------|
//! | Stim | UDP `dst = ESP_IP`, `dport` echo port (default 8080) |
//! | Payload | `FWSTIM` + `u64` LE `seq` |
//! | Timestamps | TC egress → T0; matching echo/bounce → T1 |
//!
//! # Build / deploy
//!
//! ```bash
//! cd xpost-test && cargo build -p test --release
//! scp -O target/aarch64-unknown-linux-musl/release/xpost-test tr3000:~/xpost-test
//! ```
//!
//! ## Router deps (TR3000)
//!
//! ```bash
//! opkg update && opkg install kmod-sched-bpf kmod-sched-core ip-full
//! ip link   # confirm probe.iface (often br-lan)
//! ```

#![allow(dead_code)]

/// See module-level docs (workflow hub for `xpost-test`).
pub struct Overview;
