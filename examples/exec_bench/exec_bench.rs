//! 测量 **function-call** compute（`exec` ingress；Wi-Fi-free；默认 cagt）。
//!
//! App 只是载体：不解释 opcode。结构（布局 / 期望输出）在 **本模块解析** /
//! 日后 `xpost-test` exam 里做。
//!
//! ```text
//! exec_bench (ebpf):
//!   agent flash fw exec-bench-ebpf
//!   for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
//!     agent flash xdp workload
//!     agent clear perf
//!     agent update map workload
//!     agent enable exec
//!     agent get finish
//!     agent disable exec
//!     agent get perf
//!     agent get map
//!     upload to db
//!
//! exec_bench (hal):
//!   agent flash fw exec-bench-hal
//!   for workload in {…}:
//!     agent clear perf
//!     agent update map workload
//!     agent enable exec          # run_hal + raw UART OK finish
//!     agent get finish
//!     agent disable exec
//!     agent get perf
//!     agent get map
//!     upload to db
//! ```
//!
//! 无网络刺激。主机经 agent（`xpost-serial` `Command`）驱动；map 的 key/value
//! 在串口层始终是 **字节 token**，不做结构化解码。
//!
//! - `exec` 为相对 `xdp` 的新 ingress：`enable/disable exec` 对标 `enable/disable xdp`
//! - hash map 名 `workload`：键 **input**=`0` / **output**=`1`；value 定长 32B
//! - `update map workload` 写入 input；同一字节落库为 `exec_bench_config.input`
//! - `enable exec`：eBPF 臂 `invoke`；HAL 臂 `run_hal`。成功路径 UART 出现
//!   **`OK finish`**（eBPF=`bpf_printk`；HAL=App 原始写行；**不是** serial reply）
//! - `get finish`：host-only 等 transcript 上的 `OK finish`
//! - **默认 cagt**：counter / aggregate / gauge / trace 全开
//!
//! **解析（xpost-test / 本草案）**
//!
//! 32 字节 value（LE）：
//!
//! ```text
//! [0..4]  opcode
//! [4..8]  param
//! [8..32] payload
//! ```
//!
//! opcode 名与语义见 `xpost-test` `exam/exec_bench.rs` 的 hex fixture；
//! harness 用它对 `get map` 读到的 output 字节做校验，**不要**把该抽象放进
//! App 或 `xpost-serial`。
//!
//! **数据处理**
//!
//! 每格写入 `exec_bench_config` + `exec_bench_data`（DDL：同目录
//! [`exec_bench.sql`](exec_bench.sql)）。Perf 落共享 formal / xdp_* 表。
//!
//! `exec_bench_config`：
//! - `exam_id` — 本轮 `exam` 行
//! - `firmware` — `'ebpf'` | `'hal'`
//! - `workload` — opcode / xdp profile 名（主机侧标签，非固件枚举）
//! - `input` — 本格写入 map **input** 键的原始字节
//! - `started_at` / `stopped_at` — 该格墙钟起止
//!
//! `exec_bench_data`：
//! - `config_id` — 所属 `exec_bench_config`
//! - `rep` — 重复序号（默认单轮为 1）
//! - `ok` — 本格是否成功（含 host 对 output 字节的校验）
//! - `output` — `get map` 读到的 **output** 键字节
//!
//! Draft only: parked under `examples/exec_bench/`. Harness lives in
//! `xpost-test` (`exam/exec_bench.rs`). See [`HOST_DESIGN.md`](HOST_DESIGN.md).

// Implementation deferred — no harness code in this tree.
