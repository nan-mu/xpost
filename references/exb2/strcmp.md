# strcmp（EXB2 task 6）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/strcmp.rs`。编译 Feature：`strcmp`。

## 原代码、任务语义与原生设计

原代码：[strcmp.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/strcmp.h)。

Input 包含两段各 n+1 字节的字符串，最后一个字节均为 NUL，n 范围 0..4096。按无符号字节逐对比较，至首个不同字节、相同的 NUL 或 n 对字节结束；最终固定 NUL 不计入检查数量。lo 是首个不同字节 a-b 的 32 位二进制补码，未发现差异为 0；hi 是实际检查的字节对数，包括触发早停的那一对。无 Scratch。

设计思路：可用 Rust 切片比较或优化块比较，必须返回精确差值及首个停止位置，不能只返回 Ordering 的 -1、0、1。保留内嵌 NUL、首尾差异和 n=0 语义。不修改输入。

## Agent 交互

Agent 通过 firmware profile 选择启用 `strcmp` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 6，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n 为每段字符串的最大比较长度，Input bytes=2(n+1)，Scratch bytes=0。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。无 Scratch。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 4 | abcz + NUL; abdz + NUL | 0xffffffff, 3 | 0x664ce84e |

校验完整结果缓冲区及未修改字节。无 Scratch。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
