# exec_bench — libbpf-mcu `.c` 源文件说明

交给 libbpf-mcu 侧编写 / 编译的 AOT XDP 源。目标产物经 xpost Compact ELF 打包后，由 DUT 上 `ext::exec_call::invoke`（App `enable exec`）调用，**不是** Wi-Fi RX 路径上的常规 XDP。

参考现有正式程序风格：[`examples/firewall/firewall.c`](../../firewall/firewall.c)（`#include <linux/bpf.h>` / `bpf_helpers.h` / `SEC(".maps")` / `SEC("xdp")`）。

---

## 1. 目标与边界

| 项 | 要求 |
|----|------|
| 入口 | `SEC("xdp")`，标准 `struct xdp_md *ctx` |
| 调用方式 | 主机刷冷镜像后，固件 `jalr` 进 IRAM；`ctx->data`…`data_end` 是 App 准备的 **invoke 缓冲**，不是网卡帧 |
| 完成信号 | 成功路径必须用 **`bpf_printk`（trace_printk）** 打出一行 **`OK finish`**（无前缀、无参数）。主机 `GetFinish` 在 UART transcript 上等这一行。**不要**依赖 App/serial server 发 finish |
| 失败 | 参数不合法 / 未知 opcode → `XDP_ABORTED`，**不要**打 `OK finish` |
| 成功 | 计算完成并写好 output 后 → `bpf_printk("OK finish")` → `return XDP_PASS` |
| License | `char LICENSE[] SEC("license") = "GPL";`（与 firewall 一致） |

当前仓库里有一份 **freestanding 占位** [`exec_bench.c`](exec_bench.c)（手写 helper 地址、不走 libbpf）。请用 **libbpf-mcu 正规 C** 重写并替换构建链路；语义以本文为准。

---

## 2. Map：`workload`

```c
struct {
	__uint(type, BPF_MAP_TYPE_HASH);
	__uint(max_entries, 2);
	__type(key, __u8);
	__type(value, __u8[32]);   /* 或等价 32 字节 struct */
} workload SEC(".maps");
```

| Key (`__u8`) | 含义 |
|--------------|------|
| `0` | **input** — 主机 `update map` 写入的参数块 |
| `1` | **output** — 程序写回的结果块 |

约束（Compact ELF V0 / 现网打包）：

- `value` **固定 32 字节**（布局 bit 上限 256）
- 名字必须是 **`workload`**（与 App / agent / profiles 一致）
- `max_entries = 2` 足够

### 推荐数据面路径（请按此实现）

1. `lookup(workload, key=0)` → 得到 32 字节 input；缺失则 `XDP_ABORTED`
2. 按下方 **缓冲布局** 解析 opcode / param / payload（可直接在 map value 上算，或拷到栈上）
3. 计算结果写入本地 32 字节块
4. `bpf_map_update_elem(&workload, &key1, &out, BPF_ANY)`（`key1 = 1`）
5. `bpf_printk("OK finish")`（或等价：格式串恰好产生 transcript 行 `OK finish`）
6. `return XDP_PASS`

说明：App 是 **不透明载体**——只把 map key=0 的字节拷进 `ctx` 再 `invoke`，并把返回缓冲写回 key=1；**不解析 opcode**。布局与期望输出的解析在主机 / 日后 xpost-test（见 [`HOST_DESIGN.md`](../HOST_DESIGN.md)）。**以 map 为权威 I/O**；若同时改 `ctx` 可以，但 **output 必须以 map key=1 为准**。

---

## 3. 缓冲 / value 布局（LE）

与 HAL 孪生 [`exec_bench_common`](../../exec_bench_common/src/lib.rs) 及主机解析草案一致；**整块 32 字节**：

```text
offset  size  field
0       4     opcode   (u32 LE)
4       4     param    (u32 LE；含义随 opcode)
8       24    payload / scratch
```

`opcode` 枚举：

| 名 | 值 | 行为摘要 |
|----|----|----------|
| `incr` | 0 | `payload[0..4]` 当作 u32，写回 `+1` |
| `square` | 1 | `payload[0..4]` u32，写回平方 |
| `bitswap` | 2 | `payload[0..4]` 做 32-bit bitswap，写回 |
| `fib` | 3 | 用 `param` 作 n（上限 47），结果 u32 写入 `payload[0..4]` |
| `buf` | 4 | 对前 `param` 字节做 `b[i] += i`，累加和；若空间够，和写在 `payload+param` 起 4 字节 |
| `memcpy` | 5 | 要求 `param > 0` 且 `param <= avail/2`；把前 `param` 字节拷到 `payload+param` |
| `fletcher32` | 6 | 对前 `param` 字节算 fletcher32，和写在 `payload+param` 起 4 字节 |
| `bsort` | 7 | `param = n`（2..64，且 `avail >= n*4`）；对 `payload` 上 n 个 u32 冒泡排序 |

边界：

- `data + 8 > data_end`（或 map value 不足 8 字节）→ abort
- 各 opcode 的 `avail` / `param` 检查失败 → abort
- 未知 `opcode` → abort
- 注意 value 只有 **24 字节 payload**：`buf`/`memcpy`/`fletcher32`/`bsort` 的 `param` 必须落在 24 字节可承载范围内（例如 `bsort` 的 n 最大 6；`buf`/`memcpy`/`fletcher32` 的 param 相应收紧）。请在实现里按 32 字节 value **显式收紧**，不要假设 512 字节缓冲。

参考 freestanding 占位里的算法细节：同目录 [`exec_bench.c`](exec_bench.c)（仅算法，勿沿用其 helper 手写调用）。

---

## 4. `bpf_printk` / `OK finish`

- 成功且已 `update` output 之后调用
- Transcript 上必须出现可被主机解析的一行：`OK finish`（后面换行）
- 不要加 `XDP:` 等前缀（runtime 若另有 log 前缀与这条 raw 行无关；xpost helper 会把格式化正文再打到 UART）
- 无格式参数即可，例如：`bpf_printk("OK finish");`（以实现侧实际 helper 宏为准；格式串内容必须是这 9 个字符）

---

## 5. 构建与交付

期望流程（对齐 firewall）：

1. libbpf-mcu 编译出 RISC-V AOT（如 `exec_bench.bin` / `esp32c3.elf` 调试用）
2. 带 map 的 `bpf.toml`（或你们流水线生成的等价元数据）交给 xpost
3. `xpost-compact-elf` 打成冷区 **`examples/exec_bench/exec_bench.elf`**

链接 / 槽位约定（ESP32-C3 / 本 board）：

- Helper trampoline：`0x40380000`
- XDP 槽入口（slot）：`0x40380200`
- 单镜像上限：`0xE00`（与 `examples/exec_bench` slot 一致）

`bpf.toml` 中 map 段需能被 `xpost-compact-elf` 吃进 Compact ELF V0（`map_type` HASH、`key_size`/`value_size`、`map_vma`/`aot_addr` 与 AOT 一致）。可先对齐仓库草稿 [`bpf.toml`](bpf.toml)，再按真实 AOT 链接地址改 `aot_addr`。

主机侧 profile：多名 `[[xdp]]`（`incr`…`bsort`）暂共用同一 `exec_bench.elf`；日后可拆成每 workload 一个 SEC/镜像，但 **第一版一个 dispatcher 即可**。

---

## 6. 主机侧流水线（背景，不必在 .c 里实现）

```text
agent flash fw exec-bench-ebpf
for workload in {incr, square, bitswap, fib, buf, memcpy, fletcher32, bsort}:
  agent flash xdp <workload>
  agent clear perf
  agent update map workload   # key=0, value=32B input
  agent enable exec           # App: 保证 lease → invoke 本镜像
  agent get finish            # 等 UART「OK finish」
  agent disable exec
  agent get perf
  agent get map               # 读 key=1 output
```

---

## 7. 验收清单

- [ ] libbpf-mcu 风格源：helpers + `SEC(".maps")` + `SEC("xdp")`
- [ ] map 名 `workload`，key `u8` 0/1，value 32B
- [ ] 从 input 读、算完写 output
- [ ] 成功：`OK finish` + `XDP_PASS`；失败：`XDP_ABORTED` 且无 finish
- [ ] 8 个 opcode 语义与上表一致，并适配 24B payload 上限
- [ ] 产出可被打成 `exec_bench.elf` 的 AOT + 正确 map 元数据
- [ ] 镜像 ≤ `0xE00`，helper/slot 地址符合 board 约定

---

## 8. 非目标（本文件不要求）

- 在 App / `xpost-serial` 里增加 opcode 结构化类型（解析属主机 / xpost-test）
- Wi-Fi / 以太网包解析
- 扩展 serial / agent 协议
- 拆成 8 个独立 `.c`（可选后续）

HAL 对照（同流水线、无 XDP）见 [`examples/exec_bench_hal`](../../exec_bench_hal)。
