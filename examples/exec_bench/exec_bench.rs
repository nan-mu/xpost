//! 测量 **function-call eBPF**（`exec` ingress）的 compute 耗时（Wi-Fi-free；默认 cagt）。
//!
//! ```text
//! exec_bench:
//!   agent flash fw fw
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
//! ```
//!
//! 无网络刺激。主机经 agent（`xpost-serial` `Command`）驱动：
//! `flash fw` / `flash xdp` → `clear perf` → `update map` → `enable exec` →
//! host-only `get finish` → `disable exec` → `get perf` / `get map`。
//!
//! - `exec` 为相对 `xdp` 的新 ingress：`enable/disable exec` 对标 `enable/disable xdp`
//! - hash map 名 `workload`：键 **input**=`0` / **output**=`1`；value 定长缓存
//! - `update map workload` 写入 input；同一字节落库为 `exec_bench_config.input`
//! - `enable exec` 触发 `ext::exec_call::invoke`；成功路径上 eBPF `bpf_printk`
//!   在 UART 打出 **`OK finish`**（**不是** serial server 发出）
//! - `get finish`：host-only 等 transcript 上的 `OK finish`（对标 `GetInit`）
//! - **默认 cagt**：counter / aggregate / gauge / trace 全开
//!
//! **数据处理**
//!
//! 每格写入 `exec_bench_config` + `exec_bench_data`（DDL：同目录
//! [`exec_bench.sql`](exec_bench.sql)）。Perf 落共享 formal / xdp_* 表。
//!
//! `exec_bench_config`：
//! - `exam_id` — 本轮 `exam` 行
//! - `firmware` — `'ebpf'`（本草案无 HAL 臂）
//! - `workload` — opcode / xdp profile 名
//! - `input` — 本格写入 map **input** 键的原始字节
//! - `started_at` / `stopped_at` — 该格墙钟起止
//!
//! `exec_bench_data`：
//! - `config_id` — 所属 `exec_bench_config`
//! - `rep` — 重复序号（默认单轮为 1）
//! - `ok` — 本格是否成功
//! - `output` — `get map` 读到的 **output** 键字节
//!
//! Draft only: parked under `examples/exec_bench/`; **not** wired into
//! `xpost-test`. See [`HOST_DESIGN.md`](HOST_DESIGN.md).

// Implementation deferred — no harness code in this tree.
