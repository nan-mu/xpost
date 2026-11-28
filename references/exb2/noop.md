# noop（EXB2 task 0）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/noop.rs`。编译 Feature：`noop`。

## 原代码、任务语义与原生设计

原代码：[noop.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/noop.h)。

原内核仅令 result.lo = 0、result.hi = 0 并返回 EB_OK。外层仍解析请求、执行 rounds、更新 digest、发布结果。任务没有算法输入和 Scratch，n 必须为 0。

设计思路：直接实现无计算的原生任务，保留必要的调用及结果契约。不要增加无意义的数据扫描或伪计算来模仿 eBPF 指令。若后续测量真正空入口，应与目前包含 EXB2 协议的 noop 区分，不在此处擅自改变计时定义。

## Agent 交互

Agent 通过 firmware profile 选择启用 `noop` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 0，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n=0，Input bytes=0，Scratch bytes=0。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。无 Scratch。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 0 | 无 | 0, 0 | 0x91efd7d5 |

校验完整结果缓冲区及未修改字节。无 Scratch。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
