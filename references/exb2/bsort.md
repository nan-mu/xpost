# bsort（EXB2 task 5）

对应 Rust 实现：`examples/exec_bench_hal/src/workloads/bsort.rs`。编译 Feature：`bsort`。

## 原代码、任务语义与原生设计

原代码：[bsort.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/bsort.h)。

输入 n 个 little-endian u32，n 范围 0..128。Scratch 容量为 4n 字节。原内核每轮先恢复 Input 到 Scratch，再执行完整的冒泡排序，不做已排序早停。任务结果为 Scratch 中非降序排列的全部元素，lo=最小值、hi=最大值，n=0 时均为 0。原 Input 保持不变；每轮从原始输入开始，不能利用上一轮排序后的结果减少工作。

设计思路：以原生 Rust 排序任务为基线，允许优化实现和数据组织。记录所选算法以及与原冒泡排序的区别；若采用不同排序算法，结论只能称任务级排序对照，不能声称是在比较同一个冒泡排序内核的编译效率。维护者要求任务等价，不要求复制原 C 循环。不得仅计算最小值和最大值而不实际生成排序数组。

## Agent 交互

Agent 通过 firmware profile 选择启用 `bsort` Feature 的 Firmware ELF；固件启动后只执行该 workload。当前 EXB2 兼容请求的 task ID 为 5，其他 task ID 只用于拒绝错误请求，不用于选择算法。

- 请求参数：n 为 u32 元素数，Input bytes=4n，Scratch bytes=4n。
- Agent 使用 `exec batch reset`、`exec batch write` 上传输入，再发送 `exec batch invoke`。固件执行编译时选定的任务，按现有测量契约返回时间戳和状态。Agent 使用 `exec batch read` 读取结果；读取不再次执行任务。
- 成功时 HAL status=0，completed=rounds；lo、hi 和 digest 符合任务契约。Input 和 Padding 保持不变。Scratch 必须包含排序后的全部元素；历史向量为 little-endian u32 [1, 2, 8, 8]。

共享控制流程、缓冲区布局、计时边界和构建说明见本例 README；任务书交付中的公共参考为 `references/hal-exb2-design.md`。这些是当前兼容接口，不预先批准后续协议改版或正式采样参数。

## 已有正确性向量

来源：[xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)。rounds=3、seed=1、variant=0。Scratch 初始填充 0xa5，Padding 填充 0xcc。下表是独立于新 HAL 实现的历史期望，不是新的性能输入规模。

| n | Input | 期望 lo, hi（u32） | Digest |
| ---: | --- | --- | --- |
| 4 | u32 [8, 2, 1, 8] | 1, 8 | 0x91efbaee |

校验完整结果缓冲区及未修改字节。Scratch 必须包含排序后的全部元素；历史向量为 little-endian u32 [1, 2, 8, 8]。 新实现还需按任务语义检查代表性输入；本文未运行构建或实机实验。
