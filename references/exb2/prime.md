# prime（EXB2 task 1）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/prime.rs`。编译 Feature：`prime`。

## 原代码、任务语义与原生设计

原代码：[prime.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/prime.h)。

原算法从 2 遍历至 n（含 n），以试除法判断素数；除数 d 的上限条件为 d <= x/d。任务输出区间内素数数量和最大素数；没有素数时两者为 0。无 Input 或 Scratch，n 范围 0..8192。

设计思路：使用适合 ESP32-C3 的 Rust 素数计算。可选择优化试除、奇数筛选或适合内存约束的其他计算方法；算法所需真实计算与初始化不能移出约定计时区间或用预存的固定向量答案代替。

## Agent 交互

Agent 通过 firmware profile 选择启用 `prime` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 1，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n 为素数搜索的上界，Input bytes=0，Scratch bytes=0。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。无 Scratch。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 19 | 无 | 8, 19 | 0x91efb91a |

校验完整结果缓冲区及未修改字节。无 Scratch。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
