# fletcher32（EXB2 task 4）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/fletcher32.rs`。编译 Feature：`fletcher32`。

## 原代码、任务语义与原生设计

原代码：[fletcher32.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/fletcher32.h)。

输入 n 字节，n 范围 0..4096，无 Scratch。按 little-endian 16 位字累积 Fletcher-32，两个累加器初值为 0xffff；奇数长度的最后一个字节补零高字节。原内核每 360 个完整字折叠累加器，末尾再次折叠，避免 u32 溢出。输出 lo=(s2 << 16) | s1、hi=0。

设计思路：Rust 实现可调整分块、展开循环、优化对齐读取或使用更宽的累加器。保持原有 0xffff 初值、折叠语义、奇数末字节和输出位模式。不能直接换成 CRC 或另一种 Fletcher 初值/字节序。至少用奇数、偶数、零长度和较长输入检查差异。

## Agent 交互

Agent 通过 firmware profile 选择启用 `fletcher32` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 4，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n 为校验的字节数，Input bytes=n，Scratch bytes=0。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。无 Scratch。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 3 | 01 02 03（hex） | 0x04050204, 0 | 0x199d264a |

校验完整结果缓冲区及未修改字节。无 Scratch。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
